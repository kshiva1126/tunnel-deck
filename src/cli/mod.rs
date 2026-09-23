use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};

use std::{
    collections::HashSet,
    env,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    application::hosts::{ConnectionOutcome, HostCatalog},
    application::import::{ImportClassification, ImportForwarding, selected_rules},
    daemon::{
        lifecycle::{self, DaemonEndpoint},
        manager::DaemonManager,
    },
    error::AppError,
    ipc::{Client, DEFAULT_TIMEOUT, Operation, Request, Response},
    logging::{DEFAULT_BACKUPS, DEFAULT_MAX_BYTES, RotatingLog},
    platform::{
        CURRENT,
        paths::{Paths, XdgOverrides},
        private_fs::current_uid,
    },
};

#[derive(Debug, Parser)]
#[command(
    name = "tdeck",
    version,
    about = "Manage SSH port forwarding",
    long_about = "TunnelDeck manages SSH hosts and Local forwarding plus Remote and Dynamic forwarding through a terminal UI, per-user daemon, and scriptable CLI."
)]
pub struct Cli {
    /// Emit one JSON value for automation
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect SSH hosts
    Host {
        #[command(subcommand)]
        command: HostCommand,
    },
    /// Manage forwarding rules
    Forward {
        #[command(subcommand)]
        command: ForwardCommand,
    },
    /// Show daemon and tunnel status
    Status,
    /// Inspect or change application settings
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
    /// Generate shell completion to standard output
    Completion(CompletionArgs),
    /// Generate the tdeck(1) manual page to standard output
    Manpage,
    /// Internal daemon commands
    #[command(hide = true)]
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Debug, Args)]
struct CompletionArgs {
    /// Shell whose completion script should be generated
    #[arg(value_enum)]
    shell: clap_complete::Shell,
}

#[derive(Debug, Subcommand)]
enum SettingsCommand {
    /// Show persisted application settings
    Show,
    /// Replace selected application setting values
    Set(SettingsArgs),
}

#[derive(Debug, Args)]
struct SettingsArgs {
    #[arg(long, value_enum)]
    theme: Option<ThemeArg>,
    #[arg(long, value_enum)]
    log_level: Option<LogLevelArg>,
    #[arg(long, value_parser = clap::value_parser!(bool))]
    default_reconnect: Option<bool>,
    #[arg(long, value_parser = clap::value_parser!(bool))]
    default_auto_start: Option<bool>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ThemeArg {
    System,
    Dark,
    Light,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum LogLevelArg {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Subcommand)]
enum HostCommand {
    /// List discovered SSH host aliases
    List,
    /// Show effective settings for an SSH host alias
    Show(HostAliasArgs),
    /// Test non-interactive SSH connectivity
    Test(HostAliasArgs),
}

#[derive(Debug, Args)]
struct HostAliasArgs {
    /// Exact SSH Host alias
    alias: String,
}

#[derive(Debug, Subcommand)]
enum ForwardCommand {
    /// List forwarding rules
    List,
    /// Add a Local, Remote, or Dynamic forwarding rule
    Add(AddArgs),
    /// Preview effective SSH forwards and optionally save selected candidates
    Import(ImportArgs),
    /// Remove a stopped forwarding rule
    Remove(RuleArgs),
    /// Start a forwarding rule
    Start(RuleArgs),
    /// Stop a forwarding rule
    Stop(RuleArgs),
}

#[derive(Debug, Args)]
struct ImportArgs {
    /// Exact SSH Host alias whose effective forwards are inspected
    host: String,
    /// Candidate ID to save; repeat the option or use comma-separated IDs
    #[arg(long, value_delimiter = ',')]
    select: Vec<usize>,
}

#[derive(Debug, Args)]
struct RuleArgs {
    /// Exact rule name or UUID
    rule: String,
}

#[derive(Debug, Args)]
struct AddArgs {
    #[arg(long, value_enum, default_value_t = ForwardKind::Local)]
    kind: ForwardKind,
    #[arg(long)]
    name: String,
    #[arg(long = "host")]
    ssh_host_alias: String,
    #[arg(long, default_value = "127.0.0.1")]
    bind_address: String,
    #[arg(long)]
    bind_port: u16,
    #[arg(long, default_value = "127.0.0.1")]
    destination_host: String,
    #[arg(long)]
    destination_port: Option<u16>,
    #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::value_parser!(bool))]
    auto_start: Option<bool>,
    #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::value_parser!(bool))]
    reconnect: Option<bool>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ForwardKind {
    Local,
    Remote,
    Dynamic,
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Run the per-user daemon
    Run,
    /// Supervise one private SSH attempt (internal only)
    #[command(hide = true)]
    Guardian(GuardianArgs),
}

#[derive(Debug, Args)]
struct GuardianArgs {
    #[arg(long)]
    lease_fd: i32,
    #[arg(long)]
    lock_fd: i32,
}

impl Cli {
    pub fn execute(self) -> Result<(), AppError> {
        let json_output = self.json;
        match self.command {
            None => crate::tui::run(),
            Some(Command::Host { command }) => execute_host(command, json_output),
            Some(Command::Forward { command }) => match command {
                ForwardCommand::List => {
                    daemon_call(Operation::ForwardList, serde_json::json!({}), json_output)
                }
                ForwardCommand::Add(args) => {
                    let settings =
                        daemon_value(Operation::SettingsGet, serde_json::json!({}), json_output)?;
                    let kind = match args.kind {
                        ForwardKind::Local => "local",
                        ForwardKind::Remote => "remote",
                        ForwardKind::Dynamic => "dynamic",
                    };
                    if !matches!(args.kind, ForwardKind::Dynamic) && args.destination_port.is_none()
                    {
                        return Err(AppError::Configuration(
                            "--destination-port is required for local and remote forwarding".into(),
                        ));
                    }
                    let mut payload = serde_json::json!({
                        "kind": kind,
                        "id": uuid::Uuid::new_v4(),
                        "name": args.name,
                        "ssh_host_alias": args.ssh_host_alias,
                        "bind_address": args.bind_address,
                        "bind_port": args.bind_port,
                        "auto_start": args.auto_start.unwrap_or_else(|| settings["default_auto_start"].as_bool().unwrap_or(false)),
                        "reconnect": args.reconnect.unwrap_or_else(|| settings["default_reconnect"].as_bool().unwrap_or(false)),
                    });
                    if !matches!(args.kind, ForwardKind::Dynamic) {
                        payload["destination_host"] = serde_json::json!(args.destination_host);
                        payload["destination_port"] =
                            serde_json::json!(args.destination_port.unwrap());
                    }
                    daemon_call(Operation::ForwardAdd, payload, json_output)
                }
                ForwardCommand::Import(args) => execute_import(args, json_output),
                ForwardCommand::Remove(args) => {
                    daemon_rule_call(Operation::ForwardRemove, args.rule, json_output)
                }
                ForwardCommand::Start(args) => {
                    daemon_rule_call(Operation::ForwardStart, args.rule, json_output)
                }
                ForwardCommand::Stop(args) => {
                    daemon_rule_call(Operation::ForwardStop, args.rule, json_output)
                }
            },
            Some(Command::Status) => {
                daemon_call(Operation::Status, serde_json::json!({}), json_output)
            }
            Some(Command::Settings { command }) => match command {
                SettingsCommand::Show => {
                    daemon_call(Operation::SettingsGet, serde_json::json!({}), json_output)
                }
                SettingsCommand::Set(args) => update_settings(args, json_output),
            },
            Some(Command::Completion(args)) => {
                let mut command = Self::command();
                let mut output = Vec::new();
                clap_complete::generate(args.shell, &mut command, "tdeck", &mut output);
                emit_generated("completion", output, json_output)
            }
            Some(Command::Manpage) => {
                let mut output = Vec::new();
                let command = Self::command();
                clap_mangen::Man::new(command.clone())
                    .render(&mut output)
                    .map_err(|error| {
                        AppError::Configuration(format!("could not generate manual page: {error}"))
                    })?;
                append_subcommands(&command, "tdeck", &mut output);
                emit_generated("manpage", output, json_output)
            }
            Some(Command::Daemon { command }) => match command {
                DaemonCommand::Run => run_daemon(),
                DaemonCommand::Guardian(args) => {
                    crate::daemon::process::run_guardian(args.lease_fd, args.lock_fd)
                        .map_err(|error| AppError::Ipc(error.to_string()))
                }
            },
        }
    }
}

fn append_subcommands(command: &clap::Command, prefix: &str, output: &mut Vec<u8>) {
    if command.get_subcommands().next().is_none() {
        return;
    }
    output.extend_from_slice(b"\n.SH SUBCOMMANDS\n");
    append_subcommand_entries(command, prefix, output);
}

fn append_subcommand_entries(command: &clap::Command, prefix: &str, output: &mut Vec<u8>) {
    for subcommand in command
        .get_subcommands()
        .filter(|value| !value.is_hide_set())
    {
        let name = format!("{prefix} {}", subcommand.get_name());
        output.extend_from_slice(b".TP\n\\fB");
        output.extend_from_slice(name.as_bytes());
        output.extend_from_slice(b"\\fR\n");
        if let Some(about) = subcommand.get_about() {
            output.extend_from_slice(about.to_string().as_bytes());
            output.push(b'\n');
        }
        append_subcommand_entries(subcommand, &name, output);
    }
}

fn emit_generated(kind: &str, output: Vec<u8>, json_output: bool) -> Result<(), AppError> {
    let mut stdout = std::io::stdout().lock();
    if json_output {
        let content = String::from_utf8(output).map_err(|error| {
            AppError::Configuration(format!("generated {kind} is not UTF-8: {error}"))
        })?;
        serde_json::to_writer(
            &mut stdout,
            &serde_json::json!({"kind": kind, "content": content}),
        )
        .map_err(|error| AppError::Configuration(error.to_string()))?;
        writeln!(stdout).map_err(|error| AppError::Configuration(error.to_string()))
    } else {
        stdout
            .write_all(&output)
            .map_err(|error| AppError::Configuration(error.to_string()))
    }
}

fn update_settings(args: SettingsArgs, json_output: bool) -> Result<(), AppError> {
    let mut settings = daemon_value(Operation::SettingsGet, serde_json::json!({}), json_output)?;
    if let Some(value) = args.theme {
        settings["theme"] = serde_json::json!(match value {
            ThemeArg::System => "system",
            ThemeArg::Dark => "dark",
            ThemeArg::Light => "light",
        });
    }
    if let Some(value) = args.log_level {
        settings["log_level"] = serde_json::json!(match value {
            LogLevelArg::Error => "error",
            LogLevelArg::Warn => "warn",
            LogLevelArg::Info => "info",
            LogLevelArg::Debug => "debug",
            LogLevelArg::Trace => "trace",
        });
    }
    if let Some(value) = args.default_reconnect {
        settings["default_reconnect"] = serde_json::json!(value);
    }
    if let Some(value) = args.default_auto_start {
        settings["default_auto_start"] = serde_json::json!(value);
    }
    daemon_call(Operation::SettingsUpdate, settings, json_output)
}

fn execute_import(args: ImportArgs, json_output: bool) -> Result<(), AppError> {
    let listed = daemon_value(Operation::ForwardList, serde_json::json!({}), json_output)?;
    let existing = serde_json::from_value::<Vec<crate::config::Rule>>(listed)
        .map_err(|error| AppError::Configuration(error.to_string()))?
        .into_iter()
        .map(crate::domain::rule::Rule::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let status = daemon_value(Operation::Status, serde_json::json!({}), json_output)?;
    let running = status["forwards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|value| value["state"].as_str() != Some("stopped"))
        .filter_map(|value| value["rule_id"].as_str())
        .filter_map(|value| uuid::Uuid::parse_str(value).ok())
        .map(crate::domain::rule::RuleId::from_uuid)
        .collect::<Vec<_>>();

    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| AppError::Configuration("HOME is not set".to_owned()))?;
    let ssh_home = home.join(".ssh");
    let ssh = env::var_os("TDECK_SSH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ssh"));
    let catalog = HostCatalog::new(ssh_home.join("config"), &ssh_home, ssh);
    let preview = catalog.import_preview(&args.host, &existing, &running)?;
    let mut output = import_preview_json(&preview);

    if !args.select.is_empty() {
        let selected = args.select.iter().copied().collect::<HashSet<_>>();
        if selected.len() != args.select.len() || selected.contains(&0) {
            return Err(AppError::Configuration(
                "candidate IDs must be unique positive integers".to_owned(),
            ));
        }
        if selected.iter().any(|id| *id > preview.candidates.len()) {
            return Err(AppError::Configuration(
                "selected candidate ID does not exist".to_owned(),
            ));
        }
        let settings = daemon_value(Operation::SettingsGet, serde_json::json!({}), json_output)?;
        let settings: crate::config::Settings = serde_json::from_value(settings)
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        let rules = selected_rules(
            &preview,
            &selected,
            &existing,
            settings.default_auto_start,
            settings.default_reconnect,
        )
        .map_err(AppError::Configuration)?
        .iter()
        .map(crate::config::Rule::from)
        .collect::<Vec<_>>();
        let saved = daemon_value(
            Operation::ForwardImport,
            serde_json::json!({"rules": rules}),
            json_output,
        )?;
        output["saved_rule_ids"] = saved["rule_ids"].clone();
    }

    if json_output {
        println!("{output}");
    } else {
        print_import_preview(&output);
    }
    Ok(())
}

fn import_preview_json(preview: &crate::application::import::ImportPreview) -> serde_json::Value {
    let candidates = preview
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let (status, reason) = match &candidate.classification {
                ImportClassification::Supported => ("supported", serde_json::Value::Null),
                ImportClassification::DuplicateDirective { first_index } => (
                    "duplicate",
                    serde_json::json!({"kind":"directive","candidate_id":first_index + 1}),
                ),
                ImportClassification::DuplicateRule { rule_id, running } => (
                    "duplicate",
                    serde_json::json!({"kind":"rule","rule_id":rule_id.as_uuid(),"running":running}),
                ),
                ImportClassification::Conflict { rule_id, running } => (
                    "conflict",
                    serde_json::json!({"rule_id":rule_id.map(|id| id.as_uuid()),"running":running}),
                ),
                ImportClassification::Unsupported { reason } => (
                    "unsupported",
                    serde_json::json!({"kind":unsupported_reason_name(*reason)}),
                ),
                ImportClassification::Invalid { reason } => (
                    "invalid",
                    serde_json::json!({"kind":invalid_reason_name(*reason)}),
                ),
            };
            let mut value = match &candidate.forwarding {
                Some(ImportForwarding::Local { bind_address, bind_port, destination_host, destination_port }) => serde_json::json!({"id":index + 1,"type":"local","bind":{"address":bind_address,"port":bind_port},"destination":{"host":destination_host,"port":destination_port}}),
                Some(ImportForwarding::Remote { bind_address, bind_port, destination_host, destination_port }) => serde_json::json!({"id":index + 1,"type":"remote","bind":{"address":bind_address,"port":bind_port},"destination":{"host":destination_host,"port":destination_port}}),
                Some(ImportForwarding::Dynamic { bind_address, bind_port }) => serde_json::json!({"id":index + 1,"type":"dynamic","bind":{"address":bind_address,"port":bind_port},"destination":serde_json::Value::Null}),
                None => serde_json::json!({"id":index + 1,"type":format!("{:?}", candidate.kind).to_ascii_lowercase(),"bind":serde_json::Value::Null,"destination":serde_json::Value::Null}),
            };
            value["status"] = serde_json::json!(status);
            value["reason"] = reason;
            value
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "ssh_host_alias": preview.ssh_host_alias,
        "candidates": candidates,
        "saved_rule_ids": [],
    })
}

fn unsupported_reason_name(reason: crate::application::import::UnsupportedReason) -> &'static str {
    use crate::application::import::UnsupportedReason;
    match reason {
        UnsupportedReason::UnixSocket => "unix_socket",
        UnsupportedReason::RemoteDynamic => "remote_dynamic",
    }
}

fn invalid_reason_name(reason: crate::application::import::InvalidReason) -> &'static str {
    use crate::application::import::InvalidReason;
    match reason {
        InvalidReason::Syntax => "syntax",
        InvalidReason::BindAddress => "bind_address",
        InvalidReason::DestinationHost => "destination_host",
        InvalidReason::Port => "port",
        InvalidReason::NonUtf8Output => "non_utf8_output",
    }
}

fn print_import_preview(value: &serde_json::Value) {
    println!("host: {}", value["ssh_host_alias"].as_str().unwrap_or("-"));
    for candidate in value["candidates"].as_array().into_iter().flatten() {
        let bind = candidate["bind"].as_object().map(|bind| {
            format!(
                "{}:{}",
                bind["address"].as_str().unwrap_or("-"),
                bind["port"].as_u64().unwrap_or(0)
            )
        });
        let destination = candidate["destination"].as_object().map(|destination| {
            format!(
                " -> {}:{}",
                destination["host"].as_str().unwrap_or("-"),
                destination["port"].as_u64().unwrap_or(0)
            )
        });
        let reason = if candidate["reason"].is_null() {
            String::new()
        } else {
            format!(" {}", candidate["reason"])
        };
        println!(
            "{}\t{}\t{}\t{}{}{}",
            candidate["id"].as_u64().unwrap_or(0),
            candidate["type"].as_str().unwrap_or("-"),
            candidate["status"].as_str().unwrap_or("-"),
            bind.as_deref().unwrap_or("-"),
            destination.as_deref().unwrap_or(""),
            reason,
        );
    }
    if let Some(ids) = value["saved_rule_ids"]
        .as_array()
        .filter(|ids| !ids.is_empty())
    {
        println!("saved: {}", ids.len());
    }
}

fn resolved_paths() -> Result<Paths, AppError> {
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| AppError::Configuration("HOME is not set".to_owned()))?;
    let override_path = |name| {
        env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let config = override_path("XDG_CONFIG_HOME");
    let state = override_path("XDG_STATE_HOME");
    let runtime = override_path("XDG_RUNTIME_DIR");
    Paths::resolve(
        CURRENT,
        &home,
        current_uid(),
        XdgOverrides {
            config: config.as_deref(),
            state: state.as_deref(),
            runtime: runtime.as_deref(),
        },
    )
    .map_err(|error| AppError::Configuration(error.to_string()))
}

fn daemon_rule_call(operation: Operation, rule: String, json: bool) -> Result<(), AppError> {
    let payload = match uuid::Uuid::parse_str(&rule) {
        Ok(id) => serde_json::json!({"rule_id": id}),
        Err(_) => serde_json::json!({"rule_name": rule}),
    };
    daemon_call(operation, payload, json)
}

fn daemon_call(
    operation: Operation,
    payload: serde_json::Value,
    json_output: bool,
) -> Result<(), AppError> {
    let value = daemon_value(operation, payload, json_output)?;
    if json_output {
        println!("{value}");
    } else {
        print_human(operation, &value);
    }
    Ok(())
}

fn daemon_value(
    operation: Operation,
    payload: serde_json::Value,
    json_errors: bool,
) -> Result<serde_json::Value, AppError> {
    let paths = resolved_paths()?;
    let socket = paths
        .socket_path(CURRENT)
        .map_err(|error| AppError::Ipc(error.to_string()))?;
    let executable = env::current_exe().map_err(|error| AppError::Ipc(error.to_string()))?;
    lifecycle::ensure_running(&socket, &executable, DEFAULT_TIMEOUT)
        .map_err(|error| AppError::Ipc(error.to_string()))?;
    let request = Request::new(operation, payload);
    let response_timeout = if operation == Operation::ForwardStop {
        crate::daemon::process::STOP_RESPONSE_TIMEOUT
    } else {
        DEFAULT_TIMEOUT
    };
    let response = Client::connect(&socket, response_timeout)
        .and_then(|mut client| client.call(&request))
        .map_err(|error| AppError::Ipc(error.to_string()))?;
    match response {
        Response::Success(value) => Ok(value.result),
        Response::Failure(value) => {
            let error = value.error;
            if json_errors {
                Err(AppError::JsonDaemon {
                    code: error.code,
                    message: error.message,
                })
            } else {
                Err(AppError::Daemon {
                    code: error.code,
                    message: error.message,
                })
            }
        }
    }
}

fn print_human(operation: Operation, value: &serde_json::Value) {
    match operation {
        Operation::ForwardList => {
            if let Some(rules) = value.as_array() {
                for rule in rules {
                    println!(
                        "{}\t{}",
                        rule["id"].as_str().unwrap_or("-"),
                        rule["name"].as_str().unwrap_or("-")
                    );
                }
            }
        }
        Operation::SettingsGet | Operation::SettingsUpdate => {
            println!("theme: {}", value["theme"].as_str().unwrap_or("-"));
            println!("log-level: {}", value["log_level"].as_str().unwrap_or("-"));
            println!("default-reconnect: {}", value["default_reconnect"]);
            println!("default-auto-start: {}", value["default_auto_start"]);
        }
        _ => println!("{value}"),
    }
}

fn run_daemon() -> Result<(), AppError> {
    let paths = resolved_paths()?;
    let log = RotatingLog::open(&paths.log, DEFAULT_MAX_BYTES, DEFAULT_BACKUPS)
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let socket = paths
        .socket_path(CURRENT)
        .map_err(|error| AppError::Ipc(error.to_string()))?;
    let endpoint = match DaemonEndpoint::bind(&paths.runtime, &socket) {
        Ok(value) => value,
        Err(lifecycle::DaemonError::AlreadyRunning) => return Ok(()),
        Err(error) => return Err(AppError::Ipc(error.to_string())),
    };
    let guardian_lock = endpoint
        .guardian_lock()
        .map_err(|error| AppError::Ipc(error.to_string()))?;
    let config_directory = paths.config.parent().unwrap_or_else(|| Path::new("/"));
    let executable = env::current_exe().map_err(|error| AppError::Ipc(error.to_string()))?;
    let ssh = env::var_os("TDECK_SSH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ssh"));
    let manager = Arc::new(
        DaemonManager::open_managed(
            config_directory,
            paths.runtime.clone(),
            executable,
            ssh,
            guardian_lock,
            log,
        )
        .map_err(|error| AppError::Configuration(error.to_string()))?,
    );
    manager.start_supervisor();
    lifecycle::run(endpoint, manager).map_err(|error| AppError::Ipc(error.to_string()))
}

fn execute_host(command: HostCommand, json_output: bool) -> Result<(), AppError> {
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| AppError::Configuration("HOME is not set".to_owned()))?;
    let ssh_home = home.join(".ssh");
    let mut catalog = HostCatalog::new(ssh_home.join("config"), &ssh_home, "ssh");
    match command {
        HostCommand::List => {
            let discovery = catalog.refresh()?;
            for warning in &discovery.warnings {
                eprintln!("warning: {:?} {}", warning.kind, warning.path.display());
            }
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&discovery.aliases).expect("aliases serialize")
                );
            } else {
                for alias in discovery.aliases {
                    println!("{alias}");
                }
            }
        }
        HostCommand::Show(args) => {
            let host = catalog.effective(&args.alias)?;
            if json_output {
                println!(
                    "{}",
                    serde_json::json!({"alias": host.alias, "hostname": host.hostname, "user": host.user, "port": host.port, "identity_files": host.identity_files, "proxy_jump": host.proxy_jump, "proxy_command": host.proxy_command})
                );
            } else {
                println!("alias: {}", host.alias);
                println!("hostname: {}", host.hostname);
                println!("user: {}", host.user);
                println!("port: {}", host.port);
                for identity in host.identity_files {
                    println!("identity-file: {identity}");
                }
                if let Some(proxy_jump) = host.proxy_jump {
                    println!("proxy-jump: {proxy_jump}");
                }
                if let Some(proxy_command) = host.proxy_command {
                    println!("proxy-command: {proxy_command}");
                }
            }
        }
        HostCommand::Test(args) => {
            let outcome = catalog.test_connection(&args.alias)?;
            if outcome == ConnectionOutcome::Success {
                if json_output {
                    println!(
                        "{}",
                        serde_json::json!({"success": true, "diagnostic": outcome.diagnostic()})
                    );
                } else {
                    println!("{}", outcome.diagnostic());
                }
            } else {
                return Err(AppError::Connection(outcome.diagnostic().to_owned()));
            }
        }
    }
    Ok(())
}
