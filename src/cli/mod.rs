use clap::{Args, Parser, Subcommand};

use std::{env, path::PathBuf};

use crate::{
    application::hosts::{ConnectionOutcome, HostCatalog},
    error::AppError,
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
    /// Manage forwarding rules (not implemented)
    Forward {
        #[command(subcommand)]
        command: ForwardCommand,
    },
    /// Show daemon and tunnel status (not implemented)
    Status,
    /// Internal daemon commands (not implemented)
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
    /// List forwarding rules (not implemented)
    List,
    /// Add a forwarding rule (not implemented)
    Add,
    /// Remove a stopped forwarding rule (not implemented)
    Remove(RuleArgs),
    /// Start a forwarding rule (not implemented)
    Start(RuleArgs),
    /// Stop a forwarding rule (not implemented)
    Stop(RuleArgs),
}

#[derive(Debug, Args)]
struct RuleArgs {
    /// Rule UUID or exact unique name
    rule: String,
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Run the per-user daemon (not implemented)
    Run,
}

impl Cli {
    pub fn execute(self) -> Result<(), AppError> {
        let feature = match self.command {
            None => "TUI",
            Some(Command::Host { command }) => return execute_host(command),
            Some(Command::Forward { command }) => match command {
                ForwardCommand::List => "forward listing",
                ForwardCommand::Add => "forward creation",
                ForwardCommand::Remove(args) => {
                    let _ = args.rule;
                    "forward removal"
                }
                ForwardCommand::Start(args) => {
                    let _ = args.rule;
                    "forward start"
                }
                ForwardCommand::Stop(args) => {
                    let _ = args.rule;
                    "forward stop"
                }
            },
            Some(Command::Status) => "status reporting",
            Some(Command::Daemon { command }) => match command {
                DaemonCommand::Run => "daemon",
            },
        };

        Err(AppError::Unavailable { feature })
    }
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
