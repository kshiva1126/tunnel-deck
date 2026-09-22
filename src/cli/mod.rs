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
    about = "Manage SSH port forwarding (implementation in progress)",
    long_about = "TunnelDeck manages SSH hosts and will manage port forwarding from a TUI and scriptable CLI.\n\nSSH host listing, details, and connection tests are available; tunnel operations are not implemented yet."
)]
pub struct Cli {
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
    /// Add a forwarding rule (not implemented)
    Add,
    /// Remove a stopped forwarding rule
    Remove(RuleArgs),
    /// Start a forwarding rule
    Start(RuleArgs),
    /// Stop a forwarding rule
    Stop(RuleArgs),
}

#[derive(Debug, Args)]
struct RuleArgs {
    /// Rule UUID
    rule: String,
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
        let feature = match self.command {
            None => "TUI",
            Some(Command::Host { command }) => return execute_host(command),
            Some(Command::Forward { command }) => match command {
                ForwardCommand::List => {
                    return daemon_call(Operation::ForwardList, serde_json::json!({}));
                }
                ForwardCommand::Add => "forward creation",
                ForwardCommand::Remove(args) => {
                    return daemon_rule_call(Operation::ForwardRemove, args.rule);
                }
                ForwardCommand::Start(args) => {
                    return daemon_rule_call(Operation::ForwardStart, args.rule);
                }
                ForwardCommand::Stop(args) => {
                    return daemon_rule_call(Operation::ForwardStop, args.rule);
                }
            },
            Some(Command::Status) => return daemon_call(Operation::Status, serde_json::json!({})),
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

fn daemon_rule_call(operation: Operation, rule: String) -> Result<(), AppError> {
    let id = uuid::Uuid::parse_str(&rule)
        .map_err(|_| AppError::Configuration("rule must be a UUID".to_owned()))?;
    daemon_call(operation, serde_json::json!({"rule_id": id}))
}

fn daemon_call(operation: Operation, payload: serde_json::Value) -> Result<(), AppError> {
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
            println!("{}", value.result);
            Ok(())
        }
        Response::Failure(value) => Err(AppError::Ipc(format!(
            "{:?}: {}",
            value.error.code, value.error.message
        ))),
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

fn execute_host(command: HostCommand) -> Result<(), AppError> {
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
            for alias in discovery.aliases {
                println!("{alias}");
            }
        }
        HostCommand::Show(args) => {
            let host = catalog.effective(&args.alias)?;
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
        HostCommand::Test(args) => {
            let outcome = catalog.test_connection(&args.alias)?;
            if outcome == ConnectionOutcome::Success {
                println!("{}", outcome.diagnostic());
            } else {
                return Err(AppError::Connection(outcome.diagnostic().to_owned()));
            }
        }
    }
    Ok(())
}
