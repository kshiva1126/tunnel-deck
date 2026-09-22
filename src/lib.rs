//! TunnelDeck application library.
//!
//! Host discovery, daemon IPC, and guardian-owned OpenSSH process supervision
//! are available. The terminal UI remains deliberately unavailable.

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
