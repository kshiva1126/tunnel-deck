//! TunnelDeck application library.
//!
//! Milestone 0 freezes the command, configuration, and IPC shapes. Tunnel
//! management remains deliberately unavailable until later milestones.

pub mod application;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod domain;
pub mod error;
pub mod ipc;
pub mod logging;
pub mod platform;
pub mod tui;

use clap::Parser;

use crate::{cli::Cli, error::AppError};

/// Parse the public CLI and dispatch the selected entry mode.
pub fn run() -> Result<(), AppError> {
    let cli = Cli::parse();
    cli.execute()
}
