use std::process::Command;

fn tdeck() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tdeck"))
}

#[test]
fn help_describes_available_hosts_and_unavailable_tunnels() {
    let output = tdeck().arg("--help").output().expect("run tdeck --help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 help");
    assert!(stdout.contains("SSH host listing, details, and connection tests are available"));
    assert!(stdout.contains("not implemented"));
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
fn unfinished_operation_fails_explicitly() {
    let output = tdeck()
        .args(["forward", "add"])
        .output()
        .expect("run unfinished command");
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8(output.stderr)
            .expect("UTF-8 error")
            .contains("not implemented yet")
    );
}
