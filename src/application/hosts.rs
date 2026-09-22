use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    ffi::OsStr,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use glob::{MatchOptions, glob_with};
use thiserror::Error;

use crate::domain::host::HostAlias;

const MAX_CONFIG_FILES: usize = 256;
const MAX_INCLUDE_DEPTH: usize = 32;
const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostDiscovery {
    pub aliases: Vec<String>,
    pub warnings: Vec<DiscoveryWarning>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryWarning {
    pub path: PathBuf,
    pub kind: DiscoveryWarningKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryWarningKind {
    Missing,
    Unreadable,
    InvalidIncludePattern,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveHost {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity_files: Vec<String>,
    pub proxy_jump: Option<String>,
    pub proxy_command: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionOutcome {
    Success,
    AuthenticationFailed,
    HostKeyVerificationFailed,
    NetworkFailed,
    TimedOut,
    Failed,
}

impl ConnectionOutcome {
    pub fn diagnostic(self) -> &'static str {
        match self {
            Self::Success => "connection succeeded",
            Self::AuthenticationFailed => {
                "authentication failed; check the configured key or SSH agent"
            }
            Self::HostKeyVerificationFailed => {
                "host-key verification failed; establish or repair trust with OpenSSH outside TunnelDeck"
            }
            Self::NetworkFailed => "the SSH host could not be reached",
            Self::TimedOut => "the SSH connection test timed out",
            Self::Failed => "the SSH connection test failed; run OpenSSH directly for more detail",
        }
    }
}

#[derive(Debug, Error)]
pub enum HostError {
    #[error("SSH host alias is invalid: {0}")]
    InvalidAlias(#[from] crate::domain::validation::ValidationError),
    #[error("SSH config traversal exceeded the {limit} {kind} limit")]
    TraversalLimit { kind: &'static str, limit: usize },
    #[error("could not start OpenSSH")]
    Spawn(#[source] io::Error),
    #[error("OpenSSH effective-configuration query timed out")]
    QueryTimedOut,
    #[error("OpenSSH could not resolve effective configuration")]
    QueryFailed,
    #[error("OpenSSH returned invalid effective configuration")]
    InvalidEffectiveConfiguration,
}

/// Refreshable host state. The catalog owns only discovered aliases; OpenSSH
/// remains the authority for effective settings and connection behavior.
pub struct HostCatalog {
    config_path: PathBuf,
    ssh_home: PathBuf,
    ssh_executable: PathBuf,
    aliases: Vec<String>,
}

impl HostCatalog {
    pub fn new(
        config_path: impl Into<PathBuf>,
        ssh_home: impl Into<PathBuf>,
        ssh_executable: impl Into<PathBuf>,
    ) -> Self {
        Self {
            config_path: config_path.into(),
            ssh_home: ssh_home.into(),
            ssh_executable: ssh_executable.into(),
            aliases: Vec::new(),
        }
    }

    pub fn refresh(&mut self) -> Result<HostDiscovery, HostError> {
        let discovery = discover_hosts(&self.config_path, &self.ssh_home)?;
        self.aliases.clone_from(&discovery.aliases);
        Ok(discovery)
    }

    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }

    pub fn effective(&self, alias: &str) -> Result<EffectiveHost, HostError> {
        let alias = HostAlias::new(alias)?;
        let output = run_bounded(
            &self.ssh_executable,
            [
                OsStr::new("-G"),
                OsStr::new("--"),
                OsStr::new(alias.as_str()),
            ],
            QUERY_TIMEOUT,
        )?;
        if output.timed_out {
            return Err(HostError::QueryTimedOut);
        }
        if !output.status.success() {
            return Err(HostError::QueryFailed);
        }
        parse_effective(alias.as_str(), &output.stdout)
    }

    pub fn test_connection(&self, alias: &str) -> Result<ConnectionOutcome, HostError> {
        let alias = HostAlias::new(alias)?;
        let args = [
            "-T",
            "-n",
            "-o",
            "BatchMode=yes",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ConnectionAttempts=1",
            "--",
            alias.as_str(),
            "true",
        ];
        let output = run_bounded(
            &self.ssh_executable,
            args.iter().map(OsStr::new),
            CONNECTION_TIMEOUT,
        )?;
        if output.timed_out {
            return Ok(ConnectionOutcome::TimedOut);
        }
        if output.status.success() {
            return Ok(ConnectionOutcome::Success);
        }
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        Ok(
            if stderr.contains("permission denied")
                || stderr.contains("authentication failed")
                || stderr.contains("no supported authentication methods")
            {
                ConnectionOutcome::AuthenticationFailed
            } else if stderr.contains("host key verification failed")
                || stderr.contains("remote host identification has changed")
            {
                ConnectionOutcome::HostKeyVerificationFailed
            } else if stderr.contains("connection refused")
                || stderr.contains("no route to host")
                || stderr.contains("could not resolve hostname")
                || stderr.contains("operation timed out")
                || stderr.contains("connection timed out")
            {
                ConnectionOutcome::NetworkFailed
            } else {
                ConnectionOutcome::Failed
            },
        )
    }
}

pub fn discover_hosts(config_path: &Path, ssh_home: &Path) -> Result<HostDiscovery, HostError> {
    let mut walker = ConfigWalker {
        ssh_home,
        visited: HashSet::new(),
        aliases: BTreeSet::new(),
        warnings: Vec::new(),
    };
    walker.visit(config_path, 0)?;
    Ok(HostDiscovery {
        aliases: walker.aliases.into_iter().collect(),
        warnings: walker.warnings,
    })
}

struct ConfigWalker<'a> {
    ssh_home: &'a Path,
    visited: HashSet<PathBuf>,
    aliases: BTreeSet<String>,
    warnings: Vec<DiscoveryWarning>,
}

impl ConfigWalker<'_> {
    fn visit(&mut self, path: &Path, depth: usize) -> Result<(), HostError> {
        if depth > MAX_INCLUDE_DEPTH {
            return Err(HostError::TraversalLimit {
                kind: "include-depth",
                limit: MAX_INCLUDE_DEPTH,
            });
        }
        let canonical = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) => {
                self.warnings.push(DiscoveryWarning {
                    path: path.to_owned(),
                    kind: if error.kind() == io::ErrorKind::NotFound {
                        DiscoveryWarningKind::Missing
                    } else {
                        DiscoveryWarningKind::Unreadable
                    },
                });
                return Ok(());
            }
        };
        if !self.visited.insert(canonical.clone()) {
            return Ok(());
        }
        if self.visited.len() > MAX_CONFIG_FILES {
            return Err(HostError::TraversalLimit {
                kind: "file-count",
                limit: MAX_CONFIG_FILES,
            });
        }
        let text = match fs::read_to_string(&canonical) {
            Ok(text) => text,
            Err(_) => {
                self.warnings.push(DiscoveryWarning {
                    path: canonical,
                    kind: DiscoveryWarningKind::Unreadable,
                });
                return Ok(());
            }
        };
        for logical_line in logical_lines(&text) {
            let words = split_words(&logical_line);
            let Some((keyword, values)) = words.split_first() else {
                continue;
            };
            if keyword.eq_ignore_ascii_case("host") {
                for value in values {
                    if !value.starts_with('!')
                        && !value
                            .chars()
                            .any(|character| matches!(character, '*' | '?' | '['))
                        && HostAlias::new(value.clone()).is_ok()
                    {
                        self.aliases.insert(value.clone());
                    }
                }
            } else if keyword.eq_ignore_ascii_case("include") {
                for value in values {
                    self.include(value, depth + 1)?;
                }
            }
        }
        Ok(())
    }

    fn include(&mut self, value: &str, depth: usize) -> Result<(), HostError> {
        let expanded = if value == "~" {
            self.ssh_home
                .parent()
                .unwrap_or(self.ssh_home)
                .to_path_buf()
        } else if let Some(rest) = value.strip_prefix("~/") {
            self.ssh_home.parent().unwrap_or(self.ssh_home).join(rest)
        } else {
            let path = Path::new(value);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.ssh_home.join(path)
            }
        };
        let pattern = expanded.to_string_lossy();
        let has_pattern = value
            .chars()
            .any(|character| matches!(character, '*' | '?' | '['));
        let paths = match glob_with(
            &pattern,
            MatchOptions {
                case_sensitive: true,
                require_literal_separator: true,
                require_literal_leading_dot: true,
            },
        ) {
            Ok(paths) => paths,
            Err(_) => {
                self.warnings.push(DiscoveryWarning {
                    path: expanded,
                    kind: DiscoveryWarningKind::InvalidIncludePattern,
                });
                return Ok(());
            }
        };
        let mut matched = false;
        for path in paths.flatten() {
            matched = true;
            self.visit(&path, depth)?;
        }
        if !matched && !has_pattern {
            self.visit(&expanded, depth)?;
        }
        Ok(())
    }
}

fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        current.push_str(line);
        if current.ends_with('\\') {
            current.pop();
        } else {
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            word.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if quote == Some(character) {
            quote = None;
        } else if quote.is_none() && matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if quote.is_none() && character == '#' {
            break;
        } else if quote.is_none() && (character.is_whitespace() || character == '=') {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(character);
        }
    }
    if escaped {
        word.push('\\');
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn parse_effective(alias: &str, bytes: &[u8]) -> Result<EffectiveHost, HostError> {
    let text = std::str::from_utf8(bytes).map_err(|_| HostError::InvalidEffectiveConfiguration)?;
    let mut values: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        values.entry(key).or_default().push(value.trim_start());
    }
    let one = |key| {
        values
            .get(key)
            .and_then(|values| values.first())
            .copied()
            .filter(|value| !value.is_empty())
    };
    let port = one("port")
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or(HostError::InvalidEffectiveConfiguration)?;
    let optional = |key| one(key).filter(|value| *value != "none").map(str::to_owned);
    Ok(EffectiveHost {
        alias: alias.to_owned(),
        hostname: one("hostname")
            .ok_or(HostError::InvalidEffectiveConfiguration)?
            .to_owned(),
        user: one("user")
            .ok_or(HostError::InvalidEffectiveConfiguration)?
            .to_owned(),
        port,
        identity_files: values
            .get("identityfile")
            .into_iter()
            .flatten()
            .map(|value| (*value).to_owned())
            .collect(),
        proxy_jump: optional("proxyjump"),
        proxy_command: optional("proxycommand"),
    })
}

struct ProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

fn run_bounded<I, S>(
    executable: &Path,
    args: I,
    timeout: Duration,
) -> Result<ProcessOutput, HostError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    use std::os::unix::process::CommandExt;

    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().map_err(HostError::Spawn)?;
    let stdout = read_bounded(child.stdout.take().expect("piped stdout"));
    let stderr = read_bounded(child.stderr.take().expect("piped stderr"));
    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().map_err(HostError::Spawn)? {
            break (status, false);
        }
        if Instant::now() >= deadline {
            // The dedicated process group also covers helper processes spawned by
            // ProxyCommand. ESRCH means the group exited between checks.
            unsafe {
                libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            break (child.wait().map_err(HostError::Spawn)?, true);
        }
        thread::sleep(Duration::from_millis(10));
    };
    Ok(ProcessOutput {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
        timed_out,
    })
}

fn read_bounded(mut reader: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let remaining = MAX_DIAGNOSTIC_BYTES.saturating_sub(kept.len());
                    kept.extend_from_slice(&buffer[..read.min(remaining)]);
                }
            }
        }
        kept
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_discovery_follows_includes_and_cycles_without_expanding_patterns() {
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ssh_home");
        let ssh_home = home.join(".ssh");
        let result = discover_hosts(&ssh_home.join("config"), &ssh_home).unwrap();
        assert_eq!(result.aliases, ["api", "db", "web"]);
        assert!(result.warnings.iter().any(|warning| {
            warning.kind == DiscoveryWarningKind::Missing && warning.path.ends_with("missing.conf")
        }));
    }

    #[test]
    fn excessive_include_depth_fails_instead_of_returning_partial_hosts() {
        let home = tempfile::tempdir().unwrap();
        let ssh_home = home.path().join(".ssh");
        fs::create_dir(&ssh_home).unwrap();
        fs::write(ssh_home.join("config"), "Include level-0.conf\n").unwrap();
        for level in 0..=MAX_INCLUDE_DEPTH {
            fs::write(
                ssh_home.join(format!("level-{level}.conf")),
                format!("Host partial-{level}\nInclude level-{}.conf\n", level + 1),
            )
            .unwrap();
        }
        assert!(matches!(
            discover_hosts(&ssh_home.join("config"), &ssh_home),
            Err(HostError::TraversalLimit {
                kind: "include-depth",
                limit: MAX_INCLUDE_DEPTH
            })
        ));
    }

    #[test]
    fn effective_settings_keep_multiple_identities_and_proxy_information() {
        let output = b"host example\nhostname 192.0.2.10\nuser deploy\nport 2222\nidentityfile ~/.ssh/one\nidentityfile ~/.ssh/two\nproxyjump bastion\nproxycommand none\n";
        let host = parse_effective("example", output).unwrap();
        assert_eq!(host.hostname, "192.0.2.10");
        assert_eq!(host.user, "deploy");
        assert_eq!(host.port, 2222);
        assert_eq!(host.identity_files, ["~/.ssh/one", "~/.ssh/two"]);
        assert_eq!(host.proxy_jump.as_deref(), Some("bastion"));
        assert_eq!(host.proxy_command, None);
    }

    #[test]
    fn tokenizer_handles_comments_quotes_equals_and_continuations() {
        let text = [
            "Host first \\",
            " second",
            "Include=\"conf.d/*.conf\" # ignored",
        ]
        .join("\n");
        let lines = logical_lines(&text);
        assert_eq!(split_words(&lines[0]), ["Host", "first", "second"]);
        assert_eq!(split_words(&lines[1]), ["Include", "conf.d/*.conf"]);
    }

    fn fake_ssh(script: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ssh");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        (directory, path)
    }

    #[test]
    fn fake_openssh_covers_details_success_authentication_and_host_key_failure() {
        let (_directory, ssh) = fake_ssh(
            r#"#!/bin/sh
if [ "$1" = "-G" ]; then
  printf 'hostname fixture.example\nuser fixture\nport 22\nidentityfile /fixture/key\nproxyjump none\nproxycommand none\n'
  exit 0
fi
case "${12}" in
  success) exit 0 ;;
  auth) printf 'Permission denied (publickey). secret-marker\n' >&2; exit 255 ;;
  unknown-key) printf 'Host key verification failed. secret-marker\n' >&2; exit 255 ;;
esac
exit 1
"#,
        );
        let catalog = HostCatalog::new("unused", "unused", ssh);
        assert_eq!(
            catalog.effective("details").unwrap().hostname,
            "fixture.example"
        );
        assert_eq!(
            catalog.test_connection("success").unwrap(),
            ConnectionOutcome::Success
        );
        let auth = catalog.test_connection("auth").unwrap();
        assert_eq!(auth, ConnectionOutcome::AuthenticationFailed);
        assert!(!auth.diagnostic().contains("secret-marker"));
        assert_eq!(
            catalog.test_connection("unknown-key").unwrap(),
            ConnectionOutcome::HostKeyVerificationFailed
        );
    }

    #[test]
    fn timeout_kills_the_openssh_process_group() {
        let started = Instant::now();
        let output = run_bounded(
            Path::new("/bin/sh"),
            ["-c", "sleep 10"],
            Duration::from_millis(30),
        )
        .unwrap();
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
