use clap::{Args, Parser, Subcommand};

use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    application::hosts::{ConnectionOutcome, HostCatalog},
    daemon::{
        lifecycle::{self, DaemonEndpoint},
        manager::DaemonManager,
    },
    error::AppError,
    ipc::{Client, DEFAULT_TIMEOUT, Operation, Request, Response},
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
    long_about = "TunnelDeck manages SSH hosts and Local forwarding through a per-user daemon and scriptable CLI.\n\nThe terminal UI and additional forwarding types remain in development."
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
    /// Internal daemon commands
    #[command(hide = true)]
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
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
    /// Add a Local forwarding rule
    Add(AddArgs),
    /// Remove a stopped forwarding rule
    Remove(RuleArgs),
    /// Start a forwarding rule
    Start(RuleArgs),
    /// Stop a forwarding rule
    Stop(RuleArgs),
}

#[derive(Debug, Args)]
struct RuleArgs {
    /// Exact rule name or UUID
    rule: String,
}

#[derive(Debug, Args)]
struct AddArgs {
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
    destination_port: u16,
    #[arg(long)]
    auto_start: bool,
    #[arg(long)]
    reconnect: bool,
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
        let feature = match self.command {
            None => "TUI",
            Some(Command::Host { command }) => return execute_host(command, json_output),
            Some(Command::Forward { command }) => match command {
                ForwardCommand::List => {
                    return daemon_call(Operation::ForwardList, serde_json::json!({}), json_output);
                }
                ForwardCommand::Add(args) => {
                    return daemon_call(
                        Operation::ForwardAdd,
                        serde_json::json!({
                            "kind": "local",
                            "id": uuid::Uuid::new_v4(),
                            "name": args.name,
                            "ssh_host_alias": args.ssh_host_alias,
                            "bind_address": args.bind_address,
                            "bind_port": args.bind_port,
                            "destination_host": args.destination_host,
                            "destination_port": args.destination_port,
                            "auto_start": args.auto_start,
                            "reconnect": args.reconnect,
                        }),
                        json_output,
                    );
                }
                ForwardCommand::Remove(args) => {
                    return daemon_rule_call(Operation::ForwardRemove, args.rule, json_output);
                }
                ForwardCommand::Start(args) => {
                    return daemon_rule_call(Operation::ForwardStart, args.rule, json_output);
                }
                ForwardCommand::Stop(args) => {
                    return daemon_rule_call(Operation::ForwardStop, args.rule, json_output);
                }
            },
            Some(Command::Status) => {
                return daemon_call(Operation::Status, serde_json::json!({}), json_output);
            }
            Some(Command::Daemon { command }) => match command {
                DaemonCommand::Run => return run_daemon(),
                DaemonCommand::Guardian(args) => {
                    return crate::daemon::process::run_guardian(args.lease_fd, args.lock_fd)
                        .map_err(|error| AppError::Ipc(error.to_string()));
                }
            },
        };

        Err(AppError::Unavailable { feature })
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
        Response::Success(value) => {
            if json_output {
                println!("{}", value.result);
            } else {
                print_human(operation, &value.result);
            }
            Ok(())
        }
        Response::Failure(value) => {
            let error = value.error;
            if json_output {
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
        _ => println!("{value}"),
    }
}

fn run_daemon() -> Result<(), AppError> {
    let paths = resolved_paths()?;
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
    let manager = DaemonManager::open_managed(
        config_directory,
        paths.runtime.clone(),
        executable,
        ssh,
        guardian_lock,
    )
    .map_err(|error| AppError::Configuration(error.to_string()))?;
    lifecycle::run(endpoint, Arc::new(manager)).map_err(|error| AppError::Ipc(error.to_string()))
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
