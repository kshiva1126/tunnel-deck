use std::process::Command;

fn tdeck() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tdeck"))
}

#[test]
fn help_describes_available_hosts_and_tunnel_commands() {
    let output = tdeck().arg("--help").output().expect("run tdeck --help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 help");
    assert!(stdout.contains("Local forwarding"));
    assert!(stdout.contains("scriptable CLI"));
    assert!(stdout.contains("host"));
    assert!(stdout.contains("forward"));
    assert!(stdout.contains("status"));
}

#[test]
fn host_list_uses_an_isolated_ssh_fixture() {
    let fixture_home =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ssh_home");
    let output = tdeck()
        .env("HOME", fixture_home)
        .args(["host", "list"])
        .output()
        .expect("list SSH hosts");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 aliases"),
        "api\ndb\nweb\n"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 warnings");
    assert!(stderr.contains("warning: Missing"));
    assert!(stderr.contains("missing.conf"));
}

#[test]
fn invalid_host_alias_is_a_usage_error() {
    let output = tdeck()
        .args(["host", "show", "not an alias"])
        .output()
        .expect("reject invalid SSH alias");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .expect("UTF-8 error")
            .contains("SSH host alias is invalid")
    );
}

#[test]
fn version_uses_package_version() {
    let output = tdeck()
        .arg("--version")
        .output()
        .expect("run tdeck --version");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 version"),
        format!("tdeck {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn forward_add_requires_its_non_interactive_arguments() {
    let output = tdeck()
        .args(["forward", "add"])
        .output()
        .expect("run unfinished command");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .expect("UTF-8 error")
            .contains("--name")
    );
}

#[test]
fn host_list_supports_json_output() {
    let fixture_home =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ssh_home");
    let output = tdeck()
        .env("HOME", fixture_home)
        .args(["--json", "host", "list"])
        .output()
        .expect("list SSH hosts as JSON");
    assert!(output.status.success());
    let aliases: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(aliases, serde_json::json!(["api", "db", "web"]));
}

#[cfg(unix)]
#[test]
fn local_rule_lifecycle_is_available_across_cli_processes() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::Path,
        process::{Child, Stdio},
        thread,
        time::{Duration, Instant},
    };

    struct Daemon(Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn isolated(root: &Path) -> Command {
        let mut command = tdeck();
        command
            .env("HOME", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_RUNTIME_DIR", root.join("runtime"))
            .env("TDECK_SSH", root.join("fake-ssh"));
        command
    }
    fn json_command(root: &Path, arguments: &[&str]) -> serde_json::Value {
        let output = isolated(root)
            .arg("--json")
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    // Keep the path short enough for macOS' Unix-domain socket limit. The
    // daemon adds an attempt UUID and OpenSSH control socket below this root.
    let root = tempfile::Builder::new()
        .prefix("td-cli-")
        .tempdir_in("/tmp")
        .unwrap();
    for directory in ["config", "state", "runtime"] {
        fs::create_dir(root.path().join(directory)).unwrap();
        fs::set_permissions(
            root.path().join(directory),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let ssh = root.path().join("fake-ssh");
    fs::write(
        &ssh,
        "#!/bin/sh\ncase \" $* \" in *\" -O \"*) exit 0;; esac\ntrap 'exit 0' TERM\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();

    let child = isolated(root.path())
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _daemon = Daemon(child);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let output = isolated(root.path())
            .args(["--json", "status"])
            .output()
            .unwrap();
        if output.status.success() {
            break;
        }
        assert!(Instant::now() < deadline, "daemon did not become ready");
        thread::sleep(Duration::from_millis(20));
    }

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let bind_port = listener.local_addr().unwrap().port().to_string();
    drop(listener);
    let added = json_command(
        root.path(),
        &[
            "forward",
            "add",
            "--name",
            "web",
            "--host",
            "server",
            "--bind-port",
            &bind_port,
            "--destination-port",
            "3000",
        ],
    );
    let id = added["rule_id"].as_str().unwrap();
    let started = json_command(root.path(), &["forward", "start", "web"]);
    assert_eq!(started["rule_id"], id);
    assert_eq!(started["changed"], true);
    assert_eq!(
        json_command(root.path(), &["forward", "start", id])["changed"],
        false
    );
    let status = json_command(root.path(), &["status"]);
    assert_eq!(status["active"], 1);
    assert_eq!(status["forwards"][0]["rule_id"], id);

    let refused = isolated(root.path())
        .args(["--json", "forward", "remove", "web"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(5));
    let error: serde_json::Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(error["error"]["code"], "conflict");

    assert_eq!(
        json_command(root.path(), &["forward", "stop", "web"])["changed"],
        true
    );
    assert_eq!(
        json_command(root.path(), &["forward", "stop", id])["changed"],
        false
    );
    let removed = json_command(root.path(), &["forward", "remove", "web"]);
    assert_eq!(removed["rule_id"], id);
}
