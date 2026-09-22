//! TunnelDeck application library.
//!
//! Host discovery and the daemon IPC foundation are available. OpenSSH tunnel
//! process supervision remains deliberately unavailable until later work.

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
