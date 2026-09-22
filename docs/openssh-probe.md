# OpenSSH design probe

Executed successfully on 2026-09-22, Linux, OpenSSH 10.5p1 / OpenSSL 3.6.4.
This is an architecture experiment, not the Rust application or a completed
implementation milestone.

macOS is now also an initial release target. These results remain Linux-only;
the experiment has not been ported to or executed on macOS. See
[platform decisions](design-decisions.md#platform-support) for the required
native macOS checks.

## Reproduce

Requires Linux, Python 3, and installed `ssh`, `sshd`, and `ssh-keygen`:

```text
python3 experiments/openssh_probe.py
```

The script creates mode-restricted temporary configuration, disposable client
and server keys, and a loopback-only unprivileged SSH server. It never reads or
edits personal SSH configuration or known-hosts files. Host-key verification
uses the generated server public key with `StrictHostKeyChecking=yes`; an
unknown-key connection is explicitly tested and rejected.

The test server uses `StrictModes=no` because its authorized-keys file is under
`/tmp`, whose ancestor mode check otherwise rejects it. This affects only the
disposable server's authorized-keys path checks, not client host-key checking.
The server accepts only the generated public key for the current user, with
password, keyboard-interactive authentication, and PAM disabled. No system SSH
service is configured or modified. Temporary keys and files are removed on exit.

## Observed results

| Check | Result |
| --- | --- |
| Host alias and Include select user, port, identity and known-hosts | Passed |
| Configured extra LocalForward suppressed | Passed |
| Private master differs from already-running configured master | Passed |
| ProxyJump to the isolated server and forwarded traffic | Passed |
| Unknown host key rejected | Passed |
| Local forwarding acknowledgment, traffic, cancellation | Passed |
| Remote forwarding acknowledgment, traffic, cancellation | Passed |
| Dynamic forwarding acknowledgment, SOCKS5 traffic, cancellation | Passed |
| Occupied Local and Remote listeners return failure | Passed |
| Remote bind rejected by server PermitListen policy | Passed |
| Missing control socket returns failure | Passed |
| Local setup succeeds while destination is unavailable | Passed |
| Active daemon SIGKILL triggers guardian EOF cleanup | Passed |
| SSH reaped, listener closed, lock available after cleanup | Passed |
| Unrelated master remains operational | Passed |

These observations support retaining the private-master / control-forward
approach and defining Active as forwarding acceptance rather than destination
health. The guardian experiment supports the lease-and-inherited-lock approach
for the tested active-state daemon crash.

## Limits and required follow-up

- This tests one OpenSSH version and loopback network. Establish a supported
  version range and repeat on release architectures before shipping.
- The guardian is a Python/fork prototype, not the Rust implementation. It uses
  a shortened 0.3-second termination grace period and Linux subreaper behavior
  in the test harness to reap its orphaned guardian. Production must verify its
  own descriptor ownership, shutdown timing, and child reaping.
- Crashes during spawn/authentication/stop, simultaneous guardian failure,
  detached custom ProxyCommand descendants, and cancellation races remain
  untested. Do not expand the cleanup guarantee beyond the design document.
- Agent rotation, encrypted-key unlock, IPv6, network interruption, and broad
  SSH Match/Include combinations are not covered.
- Ports are chosen dynamically; an unrelated process may claim one between
  selection and bind. This is an explicit integration probe, not a deterministic
  default unit test.
- Rust formatting, Clippy, and cargo tests do not apply yet: there is no
  Cargo.toml or Rust implementation. Run the required checks when scaffolding
  starts.
