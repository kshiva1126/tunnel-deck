//! Operating-system-specific behavior boundary.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Linux,
    MacOs,
}

pub const MINIMUM_LINUX_KERNEL: &str = "5.15";
pub const MINIMUM_LINUX_GLIBC: &str = "2.35";
pub const MINIMUM_MACOS: &str = "13.0";

#[cfg(target_os = "linux")]
pub const CURRENT: Platform = Platform::Linux;

#[cfg(target_os = "macos")]
pub const CURRENT: Platform = Platform::MacOs;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("TunnelDeck currently supports Linux and macOS only");

pub mod browser;
pub mod paths;
pub(crate) mod private_fs;
