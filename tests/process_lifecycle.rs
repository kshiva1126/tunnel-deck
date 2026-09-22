use std::{
    fs::{self, OpenOptions},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tunnel_deck::{
    daemon::process::{self, MAX_STDERR_BYTES},
    domain::rule::{Rule, RuleId},
};

fn private_tempdir() -> TempDir {
    #[cfg(target_os = "macos")]
    let root = tempfile::Builder::new()
        .prefix("td-")
        .tempdir_in("/tmp")
        .unwrap();
    #[cfg(not(target_os = "macos"))]
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn fake_ssh(root: &Path, master: &str) -> (PathBuf, PathBuf) {
    fake_ssh_with_check(root, master, "exit 0")
}

fn fake_ssh_with_check(root: &Path, master: &str, check: &str) -> (PathBuf, PathBuf) {
    let path = root.join("fake-ssh");
    let log = root.join("arguments.log");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'
case \" $* \" in
  *' -O check '*) {check} ;;
  *' -O forward '*) exit 0 ;;
esac
{master}\n",
        log.display()
    );
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    (path, log)
}

#[test]
fn stop_during_start_discards_the_late_forwarding_success() {
    use serde_json::json;
    use std::{sync::Arc, thread};
    use tunnel_deck::{
        config::ConfigStore,
        daemon::{lifecycle::RequestHandler, manager::DaemonManager},
        ipc::{Operation, PROTOCOL_VERSION, Request, Response},
    };

    let root = private_tempdir();
    let config = root.path().join("config");
    fs::create_dir(&config).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
    let managed_rule = rule();
    ConfigStore::open(&config)
        .unwrap()
        .save(std::slice::from_ref(&managed_rule))
        .unwrap();
    let (ssh, _) = fake_ssh_with_check(
        root.path(),
        "trap 'exit 0' TERM; while :; do sleep 1; done",
        "sleep 1; exit 0",
    );
    let manager = Arc::new(
        DaemonManager::open_managed(
            &config,
            root.path().to_owned(),
            PathBuf::from(env!("CARGO_BIN_EXE_tdeck")),
            ssh,
            lock(root.path()),
        )
        .unwrap(),
    );
    let id = managed_rule.id().as_uuid();
    let request = move |operation| Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: uuid::Uuid::new_v4(),
        operation,
        payload: json!({"rule_id": id}),
    };
    let starter = {
        let manager = Arc::clone(&manager);
        thread::spawn(move || manager.handle(&request(Operation::ForwardStart)))
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while !fs::read_dir(root.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("attempt-")
    }) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let stopped = manager.handle(&request(Operation::ForwardStop));
    assert!(matches!(stopped, Response::Success(value) if value.result["changed"] == true));
    let started = starter.join().unwrap();
    assert!(
        matches!(started, Response::Success(value) if value.result["start_requested"] == false)
    );
    let status = manager.handle(&Request::new(Operation::Status, json!({})));
    assert!(
        matches!(status, Response::Success(value) if value.result["active"] == 0 && value.result["starting"] == 0)
    );
}

#[test]
fn status_remains_available_while_stop_waits_for_guardian_cleanup() {
    use serde_json::json;
    use std::{sync::Arc, thread};
    use tunnel_deck::{
        config::ConfigStore,
        daemon::{lifecycle::RequestHandler, manager::DaemonManager},
        ipc::{Operation, Request, Response},
    };

    let root = private_tempdir();
    let config = root.path().join("config");
    fs::create_dir(&config).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
    let managed_rule = rule();
    ConfigStore::open(&config)
        .unwrap()
        .save(std::slice::from_ref(&managed_rule))
        .unwrap();
    let (ssh, _) = fake_ssh(root.path(), "trap '' TERM; while :; do sleep 1; done");
    let manager = Arc::new(
        DaemonManager::open_managed(
            &config,
            root.path().to_owned(),
            PathBuf::from(env!("CARGO_BIN_EXE_tdeck")),
            ssh,
            lock(root.path()),
        )
        .unwrap(),
    );
    let id = managed_rule.id().as_uuid();
    let request = move |operation| Request::new(operation, json!({"rule_id": id}));
    assert!(matches!(
        manager.handle(&request(Operation::ForwardStart)),
        Response::Success(value) if value.result["changed"] == true
    ));

    let stopper = {
        let manager = Arc::clone(&manager);
        thread::spawn(move || manager.handle(&request(Operation::ForwardStop)))
    };
    thread::sleep(Duration::from_millis(100));
    let status_started = Instant::now();
    let status = manager.handle(&Request::new(Operation::Status, json!({})));
    assert!(status_started.elapsed() < Duration::from_secs(1));
    assert!(matches!(
        status,
        Response::Success(value) if value.result["active"] == 0
    ));
    assert!(matches!(
        stopper.join().unwrap(),
        Response::Success(value) if value.result["changed"] == true
    ));
}

fn rule() -> Rule {
    Rule::local(
        RuleId::new(),
        "web",
        "configured-alias",
        39123,
        "127.0.0.1",
        3000,
    )
    .unwrap()
}

fn lock(root: &Path) -> std::fs::File {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("daemon.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) },
        0
    );
    file
}

#[test]
fn lease_eof_after_daemon_crash_cleans_attempt_before_releasing_lock() {
    use std::os::fd::AsRawFd;
    let root = private_tempdir();
    let (ssh, _) = fake_ssh(root.path(), "trap 'exit 0' TERM; while :; do sleep 1; done");
    let daemon_lock = lock(root.path());
    let attempt = process::start_attempt(
        Path::new(env!("CARGO_BIN_EXE_tdeck")),
        &ssh,
        root.path(),
        &daemon_lock,
        &rule(),
    )
    .unwrap();
    let attempt_dir = root.path().join(format!("attempt-{}", attempt.attempt_id));
    drop(attempt);
    drop(daemon_lock);

    let replacement = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("daemon.lock"))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let acquired =
            unsafe { libc::flock(replacement.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if acquired {
            assert!(!attempt_dir.exists());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "guardian retained the daemon lock too long"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn private_master_is_forwarded_then_forced_down_and_cleaned() {
    let root = private_tempdir();
    let descendant = root.path().join("descendant.pid");
    let emitted = MAX_STDERR_BYTES + 1024;
    let (ssh, log) = fake_ssh(
        root.path(),
        &format!(
            "sleep 1000 &\nprintf '%s\\n' \"$!\" > '{}'\ntrap '' TERM\ni=0; while [ $i -lt {emitted} ]; do printf x >&2; i=$((i+1)); done\nwhile :; do sleep 1; done",
            descendant.display()
        ),
    );
    let started = Instant::now();
    let attempt = process::start_attempt(
        Path::new(env!("CARGO_BIN_EXE_tdeck")),
        &ssh,
        root.path(),
        &lock(root.path()),
        &rule(),
    )
    .unwrap();
    let attempt_dir = root.path().join(format!("attempt-{}", attempt.attempt_id));
    assert!(attempt_dir.is_dir());
    let arguments = fs::read_to_string(log).unwrap();
    let lines: Vec<_> = arguments.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("ClearAllForwardings=yes"));
    assert!(!lines[0].contains(" -L "));
    assert!(lines[1].contains("-F /dev/null"));
    assert!(lines[1].contains("-O check"));
    assert!(lines[2].contains("-O forward"));
    assert!(lines[2].contains("ClearAllForwardings=no"));
    assert!(lines[2].contains("-L 127.0.0.1:39123:127.0.0.1:3000"));
    let descendant_pid: i32 = fs::read_to_string(&descendant)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    attempt.stop().unwrap();
    assert!(!attempt_dir.exists());
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(emitted > MAX_STDERR_BYTES);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let result = unsafe { libc::kill(descendant_pid, 0) };
        if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background SSH descendant survived process-group cleanup"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn early_master_failure_is_reported_and_attempt_directory_is_removed() {
    let root = private_tempdir();
    let (ssh, _) = fake_ssh(root.path(), "printf 'authentication failed' >&2; exit 23");
    let error = match process::start_attempt(
        Path::new(env!("CARGO_BIN_EXE_tdeck")),
        &ssh,
        root.path(),
        &lock(root.path()),
        &rule(),
    ) {
        Ok(_) => panic!("early failure unexpectedly became active"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("authentication failed"));
    assert!(fs::read_dir(root.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("attempt-")
    }));
}
