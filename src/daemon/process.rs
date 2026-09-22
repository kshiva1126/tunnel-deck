//! OpenSSH argument construction and guardian-owned process supervision.

use crate::domain::rule::{Forwarding, Rule};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;
use uuid::Uuid;

pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
pub const STOP_GRACE: Duration = Duration::from_secs(5);
pub const STOP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_STDERR_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("SSH process I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("guardian protocol failed: {0}")]
    Protocol(String),
    #[error("SSH startup failed: {0}")]
    Startup(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    Authentication,
    HostKey,
    Listener,
    Network,
    RemoteRejected,
    Unknown,
}

impl DiagnosticKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::HostKey => "host_key",
            Self::Listener => "listener",
            Self::Network => "network",
            Self::RemoteRejected => "remote_rejected",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    pub message: &'static str,
    pub retryable: bool,
}

/// Classifies only well-known OpenSSH text and returns a fixed, redacted
/// message. Unknown output is never guessed or returned to clients.
pub fn classify_diagnostic(value: &str) -> Diagnostic {
    let lower = value.to_ascii_lowercase();
    if lower.contains("permission denied") || lower.contains("authentication failed") {
        Diagnostic {
            kind: DiagnosticKind::Authentication,
            message: "SSH authentication failed",
            retryable: false,
        }
    } else if lower.contains("host key verification failed")
        || lower.contains("remote host identification has changed")
    {
        Diagnostic {
            kind: DiagnosticKind::HostKey,
            message: "SSH host-key verification failed",
            retryable: false,
        }
    } else if lower.contains("remote port forwarding failed")
        || lower.contains("administratively prohibited")
    {
        Diagnostic {
            kind: DiagnosticKind::RemoteRejected,
            message: "the SSH server rejected remote forwarding",
            retryable: false,
        }
    } else if lower.contains("address already in use") || lower.contains("cannot listen to port") {
        Diagnostic {
            kind: DiagnosticKind::Listener,
            message: "the requested listener could not be created",
            retryable: false,
        }
    } else if lower.contains("connection timed out")
        || lower.contains("connection refused")
        || lower.contains("no route to host")
        || lower.contains("connection reset")
        || lower.contains("network is unreachable")
    {
        Diagnostic {
            kind: DiagnosticKind::Network,
            message: "the SSH network connection failed",
            retryable: true,
        }
    } else {
        Diagnostic {
            kind: DiagnosticKind::Unknown,
            message: "SSH startup failed for an unclassified reason",
            retryable: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GuardianSpec {
    ssh: PathBuf,
    alias: String,
    socket: PathBuf,
    attempt_dir: PathBuf,
    forward_flag: String,
    forward_spec: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct GuardianReport {
    accepted: bool,
    diagnostic: String,
    master_pid: u32,
}

pub struct ManagedAttempt {
    lease: Option<UnixStream>,
    guardian: Option<Child>,
    master_pid: i32,
    attempt_dir: PathBuf,
    pub attempt_id: Uuid,
}

impl ManagedAttempt {
    pub fn stop(mut self) -> Result<(), ProcessError> {
        self.cleanup()
    }

    pub fn has_exited(&mut self) -> Result<bool, ProcessError> {
        Ok(match self.guardian.as_mut() {
            Some(guardian) => guardian.try_wait()?.is_some(),
            None => true,
        })
    }

    fn cleanup(&mut self) -> Result<(), ProcessError> {
        self.lease.take();
        let Some(mut guardian) = self.guardian.take() else {
            return Ok(());
        };
        match wait_child(&mut guardian, STOP_GRACE + Duration::from_secs(2)) {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => {
                self.force_cleanup(&mut guardian);
                Err(ProcessError::Protocol(format!(
                    "guardian exited unsuccessfully: {status}"
                )))
            }
            Err(error) => {
                self.force_cleanup(&mut guardian);
                Err(error)
            }
        }
    }

    fn force_cleanup(&self, guardian: &mut Child) {
        // The group belongs to this still-live attempt; no persisted or
        // externally supplied PID is used as signaling authority.
        unsafe { libc::kill(-self.master_pid, libc::SIGKILL) };
        let _ = guardian.kill();
        let _ = guardian.wait();
        let _ = fs::remove_dir_all(&self.attempt_dir);
    }
}

impl Drop for ManagedAttempt {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

pub fn start_attempt(
    executable: &Path,
    ssh: &Path,
    runtime: &Path,
    lock: &File,
    rule: &Rule,
) -> Result<ManagedAttempt, ProcessError> {
    let attempt_id = Uuid::new_v4();
    let attempt_dir = runtime.join(format!("attempt-{attempt_id}"));
    fs::create_dir(&attempt_dir)?;
    fs::set_permissions(&attempt_dir, fs::Permissions::from_mode(0o700))?;
    let socket = attempt_dir.join("control.sock");
    check_socket_length(&socket)?;
    let (daemon_lease, guardian_lease) = UnixStream::pair()?;
    let guardian_fd = guardian_lease.as_raw_fd();
    let inherited_lock = lock.try_clone()?;
    let lock_fd = inherited_lock.as_raw_fd();
    let mut command = Command::new(executable);
    command
        .args([
            "daemon",
            "guardian",
            "--lease-fd",
            &guardian_fd.to_string(),
            "--lock-fd",
            &lock_fd.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: this only changes flags on descriptors owned by this command.
    unsafe {
        command.pre_exec(move || {
            clear_cloexec(guardian_fd)?;
            clear_cloexec(lock_fd)?;
            Ok(())
        });
    }
    let mut guardian = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_dir_all(&attempt_dir);
            return Err(error.into());
        }
    };
    drop(guardian_lease);
    drop(inherited_lock);
    let (forward_flag, forward_spec) = forwarding(rule);
    let spec = GuardianSpec {
        ssh: ssh.to_owned(),
        alias: rule.ssh_host_alias().as_str().to_owned(),
        socket,
        attempt_dir,
        forward_flag,
        forward_spec,
    };
    daemon_lease.set_read_timeout(Some(
        STARTUP_TIMEOUT + CONTROL_TIMEOUT + STOP_GRACE + Duration::from_secs(2),
    ))?;
    let exchange = (|| -> Result<GuardianReport, ProcessError> {
        write_json(&daemon_lease, &spec)?;
        read_json(&mut BufReader::new(&daemon_lease))
    })();
    let report = match exchange {
        Ok(report) => report,
        Err(error) => {
            drop(daemon_lease);
            let _ = wait_child(&mut guardian, STOP_GRACE + Duration::from_secs(2));
            let _ = fs::remove_dir_all(&spec.attempt_dir);
            return Err(error);
        }
    };
    if !report.accepted {
        drop(daemon_lease);
        let _ = wait_child(&mut guardian, STOP_GRACE + Duration::from_secs(2));
        let _ = fs::remove_dir_all(&spec.attempt_dir);
        return Err(ProcessError::Startup(report.diagnostic));
    }
    let master_pid = match i32::try_from(report.master_pid).ok().filter(|pid| *pid > 0) {
        Some(pid) => pid,
        None => {
            drop(daemon_lease);
            let _ = wait_child(&mut guardian, STOP_GRACE + Duration::from_secs(2));
            let _ = fs::remove_dir_all(&spec.attempt_dir);
            return Err(ProcessError::Protocol(
                "guardian returned an invalid master PID".into(),
            ));
        }
    };
    daemon_lease.set_read_timeout(None)?;
    Ok(ManagedAttempt {
        lease: Some(daemon_lease),
        guardian: Some(guardian),
        master_pid,
        attempt_dir: spec.attempt_dir,
        attempt_id,
    })
}

fn forwarding(rule: &Rule) -> (String, String) {
    match rule.forwarding() {
        Forwarding::Local(value) => (
            "-L".into(),
            format!(
                "{}:{}:{}:{}",
                bracket(value.bind_address().as_str()),
                value.bind_port(),
                bracket(value.destination_host().as_str()),
                value.destination_port()
            ),
        ),
        Forwarding::Remote(value) => (
            "-R".into(),
            format!(
                "{}:{}:{}:{}",
                bracket(value.bind_address().as_str()),
                value.bind_port(),
                bracket(value.destination_host().as_str()),
                value.destination_port()
            ),
        ),
        Forwarding::Dynamic(value) => (
            "-D".into(),
            format!(
                "{}:{}",
                bracket(value.bind_address().as_str()),
                value.bind_port()
            ),
        ),
    }
}

fn bracket(value: &str) -> String {
    if value.contains(':') {
        format!("[{value}]")
    } else {
        value.to_owned()
    }
}

pub fn run_guardian(lease_fd: RawFd, lock_fd: RawFd) -> Result<(), ProcessError> {
    // SAFETY: the private command receives unique inherited descriptors.
    let lease = unsafe { UnixStream::from_raw_fd(lease_fd) };
    let lock = unsafe { File::from_raw_fd(lock_fd) };
    set_cloexec(lease.as_raw_fd())?;
    set_cloexec(lock.as_raw_fd())?;
    let spec: GuardianSpec = read_json(&mut BufReader::new(&lease))?;
    let outcome = guardian_start(&spec);
    let report = match &outcome {
        Ok((master, _)) => GuardianReport {
            accepted: true,
            diagnostic: String::new(),
            master_pid: master.id(),
        },
        Err(error) => GuardianReport {
            accepted: false,
            diagnostic: error.to_string(),
            master_pid: 0,
        },
    };
    if let Err(error) = write_json(&lease, &report) {
        if let Ok((mut master, stderr_task)) = outcome {
            let _ = terminate_group(&mut master, STOP_GRACE);
            let _ = stderr_task.join();
        }
        let _ = fs::remove_dir_all(&spec.attempt_dir);
        return Err(error);
    }
    let (mut master, stderr_task) = match outcome {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_dir_all(&spec.attempt_dir);
            return Err(error);
        }
    };
    lease.set_nonblocking(true)?;
    loop {
        let mut byte = [0_u8; 1];
        match (&lease).read(&mut byte) {
            Ok(0) | Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
        if master.try_wait()?.is_some() {
            let _ = stderr_task.join();
            let _ = fs::remove_dir_all(&spec.attempt_dir);
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    terminate_group(&mut master, STOP_GRACE)?;
    let _ = stderr_task.join();
    let _ = fs::remove_dir_all(&spec.attempt_dir);
    Ok(())
}

fn guardian_start(
    spec: &GuardianSpec,
) -> Result<(Child, thread::JoinHandle<Vec<u8>>), ProcessError> {
    let mut command = Command::new(&spec.ssh);
    command
        .args([
            "-N",
            "-T",
            "-n",
            "-o",
            "BatchMode=yes",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ControlMaster=yes",
            "-o",
            "ControlPersist=no",
            "-o",
            "ForkAfterAuthentication=no",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-S",
        ])
        .arg(&spec.socket)
        .arg(&spec.alias)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // SAFETY: setpgid executes in the child before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut master = command.spawn()?;
    let stderr = master.stderr.take().expect("piped stderr");
    let stderr_task = thread::spawn(move || bounded_read(stderr));
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Some(status) = master.try_wait()? {
            return Err(ProcessError::Startup(format_exit(
                status,
                join_diagnostic(stderr_task),
            )));
        }
        if control(spec, &["-O", "check"]).is_ok() {
            break;
        }
        if Instant::now() >= deadline {
            terminate_group(&mut master, STOP_GRACE)?;
            let diagnostic = join_diagnostic(stderr_task);
            return Err(ProcessError::Startup(if diagnostic.is_empty() {
                "SSH master readiness timed out".into()
            } else {
                diagnostic
            }));
        }
        thread::sleep(Duration::from_millis(50));
    }
    if let Err(error) = control(
        spec,
        &[
            "-O",
            "forward",
            "-o",
            "ClearAllForwardings=no",
            "-o",
            "ExitOnForwardFailure=yes",
            &spec.forward_flag,
            &spec.forward_spec,
        ],
    ) {
        terminate_group(&mut master, STOP_GRACE)?;
        let diagnostic = join_diagnostic(stderr_task);
        return Err(ProcessError::Startup(if diagnostic.is_empty() {
            error.to_string()
        } else {
            diagnostic
        }));
    }
    if master.try_wait()?.is_some() {
        let diagnostic = join_diagnostic(stderr_task);
        return Err(ProcessError::Startup(if diagnostic.is_empty() {
            "SSH master exited after accepting the forwarding".into()
        } else {
            diagnostic
        }));
    }
    Ok((master, stderr_task))
}

fn control(spec: &GuardianSpec, arguments: &[&str]) -> Result<(), ProcessError> {
    let mut child = Command::new(&spec.ssh)
        .args(["-F", "/dev/null", "-S"])
        .arg(&spec.socket)
        .args(arguments)
        .arg(&spec.alias)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(ProcessError::Startup(
                    "SSH control request was rejected".into(),
                ))
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProcessError::Startup(
                "SSH control request timed out".into(),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn terminate_group(child: &mut Child, grace: Duration) -> Result<(), ProcessError> {
    let pid = child.id() as i32;
    unsafe { libc::kill(-pid, libc::SIGTERM) };
    let deadline = Instant::now() + grace;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
            child.wait()?;
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn bounded_read(mut reader: impl Read) -> Vec<u8> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8192];
    while let Ok(count) = reader.read(&mut buffer) {
        if count == 0 {
            break;
        }
        let remaining = MAX_STDERR_BYTES.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    retained
}

fn join_diagnostic(task: thread::JoinHandle<Vec<u8>>) -> String {
    String::from_utf8_lossy(&task.join().unwrap_or_default())
        .trim()
        .to_owned()
}
fn format_exit(status: ExitStatus, diagnostic: String) -> String {
    if diagnostic.is_empty() {
        format!("SSH master exited with {status}")
    } else {
        diagnostic
    }
}
fn wait_child(child: &mut Child, timeout: Duration) -> Result<ExitStatus, ProcessError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(ProcessError::Protocol(
                "guardian did not finish cleanup".into(),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn write_json(mut writer: impl Write, value: &impl Serialize) -> Result<(), ProcessError> {
    serde_json::to_writer(&mut writer, value)
        .map_err(|error| ProcessError::Protocol(error.to_string()))?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
fn read_json<T: for<'de> Deserialize<'de>>(reader: &mut impl BufRead) -> Result<T, ProcessError> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 || line.len() > MAX_STDERR_BYTES * 2 {
        return Err(ProcessError::Protocol(
            "guardian disconnected or sent an oversized message".into(),
        ));
    }
    serde_json::from_str(&line).map_err(|error| ProcessError::Protocol(error.to_string()))
}
fn check_socket_length(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let limit = if cfg!(target_os = "macos") { 104 } else { 108 };
    if path.as_os_str().as_bytes().len() >= limit {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "control socket path is too long",
        ))
    } else {
        Ok(())
    }
}
fn clear_cloexec(fd: RawFd) -> io::Result<()> {
    descriptor_flag(fd, false)
}
fn set_cloexec(fd: RawFd) -> io::Result<()> {
    descriptor_flag(fd, true)
}
fn descriptor_flag(fd: RawFd, enabled: bool) -> io::Result<()> {
    let current = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if current == -1 {
        return Err(io::Error::last_os_error());
    }
    let next = if enabled {
        current | libc::FD_CLOEXEC
    } else {
        current & !libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, next) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::rule::{Rule, RuleId};
    #[test]
    fn diagnostics_are_classified_without_echoing_captured_secrets() {
        let secret = "private-token-value";
        let auth = classify_diagnostic(&format!("Permission denied {secret}"));
        assert_eq!(auth.kind, DiagnosticKind::Authentication);
        assert!(!auth.message.contains(secret));
        assert!(!auth.retryable);
        let unknown = classify_diagnostic(&format!("unexpected detail {secret}"));
        assert_eq!(unknown.kind, DiagnosticKind::Unknown);
        assert!(!unknown.message.contains(secret));
    }
    #[test]
    fn local_forward_spec_brackets_ipv6() {
        let rule = Rule::local(RuleId::new(), "web", "work", 3000, "::1", 80)
            .unwrap()
            .with_bind_address("::1")
            .unwrap();
        assert_eq!(
            forwarding(&rule),
            ("-L".into(), "[::1]:3000:[::1]:80".into())
        );
    }
    #[test]
    fn remote_and_dynamic_forward_specs_use_their_own_flags() {
        let remote = Rule::remote(RuleId::new(), "remote", "work", 8080, "::1", 80)
            .unwrap()
            .with_bind_address("0.0.0.0")
            .unwrap();
        assert_eq!(
            forwarding(&remote),
            ("-R".into(), "0.0.0.0:8080:[::1]:80".into())
        );
        let dynamic = Rule::dynamic(RuleId::new(), "socks", "work", 1080)
            .unwrap()
            .with_bind_address("::1")
            .unwrap();
        assert_eq!(forwarding(&dynamic), ("-D".into(), "[::1]:1080".into()));
    }
    #[test]
    fn stderr_is_bounded_while_fully_drained() {
        assert_eq!(
            bounded_read(vec![b'x'; MAX_STDERR_BYTES * 3].as_slice()).len(),
            MAX_STDERR_BYTES
        );
    }

    #[test]
    fn abnormal_guardian_exit_forces_master_group_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let attempt_dir = root.path().join("attempt");
        fs::create_dir(&attempt_dir).unwrap();
        let mut master_command = Command::new("sh");
        master_command.args(["-c", "while :; do sleep 1; done"]);
        unsafe {
            master_command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut master = master_command.spawn().unwrap();
        let guardian = Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap();
        let mut attempt = ManagedAttempt {
            lease: None,
            guardian: Some(guardian),
            master_pid: master.id() as i32,
            attempt_dir: attempt_dir.clone(),
            attempt_id: Uuid::new_v4(),
        };

        assert!(
            matches!(attempt.cleanup(), Err(ProcessError::Protocol(message)) if message.contains("guardian exited unsuccessfully"))
        );
        assert!(!attempt_dir.exists());
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if master.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "master survived guardian failure"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
