use clap::{Args, Parser, Subcommand};

use crate::error::AppError;

#[derive(Debug, Parser)]
#[command(
    name = "tdeck",
    version,
    about = "Manage SSH port forwarding (implementation in progress)",
    long_about = "TunnelDeck will manage SSH port forwarding from a TUI and scriptable CLI.\n\nThis build freezes the command interface; tunnel operations are not implemented yet."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect SSH hosts (not implemented)
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
    /// List discovered SSH host aliases (not implemented)
    List,
    /// Show effective settings for an SSH host alias (not implemented)
    Show(HostAliasArgs),
    /// Test non-interactive SSH connectivity (not implemented)
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
            Some(Command::Host { command }) => match command {
                HostCommand::List => "host listing",
                HostCommand::Show(args) => {
                    let _ = args.alias;
                    "host details"
                }
                HostCommand::Test(args) => {
                    let _ = args.alias;
                    "host connectivity testing"
                }
            },
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
