//! Explicit browser launching kept behind the operating-system boundary.

use std::{io, process::Command};

pub fn command_for(platform: super::Platform, url: &str) -> Command {
    let mut command = match platform {
        super::Platform::Linux => Command::new("xdg-open"),
        super::Platform::MacOs => Command::new("open"),
    };
    command.arg(url);
    command
}

/// Launch the platform browser without involving a shell.
pub fn open(url: &str) -> io::Result<()> {
    command_for(super::CURRENT, url).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn browser_commands_pass_the_url_as_one_argument() {
        let url = "https://127.0.0.1:3000/a?b=c&d=e";
        let linux = command_for(super::super::Platform::Linux, url);
        assert_eq!(linux.get_program(), OsStr::new("xdg-open"));
        assert_eq!(linux.get_args().collect::<Vec<_>>(), [OsStr::new(url)]);
        let mac = command_for(super::super::Platform::MacOs, url);
        assert_eq!(mac.get_program(), OsStr::new("open"));
        assert_eq!(mac.get_args().collect::<Vec<_>>(), [OsStr::new(url)]);
    }
}
