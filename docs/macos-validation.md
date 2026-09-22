# macOS validation

macOS support is **under validation** until every release architecture has a
native runtime result and the human checks below are complete. Cross-compiling
or passing Linux tests does not change that status.

The initial minimum is macOS 13. Release candidates target Apple Silicon
(`aarch64`) and Intel (`x86_64`). Do not lower or raise the minimum based only
on a CI runner; verify the shipped artifact on the minimum OS before release.

## Automated native checks

The `macos-latest` GitHub Actions job records `sw_vers` and `uname -m`, runs the
complete Rust suite, and then runs `experiments/openssh_probe.py`. On macOS the
probe refuses any SSH client other than Apple's `/usr/bin/ssh`. It creates
disposable keys, known-hosts data, SSH configuration, and an unprivileged
loopback sshd under a private temporary directory. It never reads personal SSH
configuration or contacts an external host.

The combined job covers:

- macOS default and XDG-overridden configuration/state/runtime paths, Unix
  socket length, and private directory/file permissions;
- inherited daemon-lock lifetime, lease-EOF guardian cleanup, process-group
  termination, and attempt-directory cleanup through the Rust lifecycle tests;
- private-master isolation, strict host-key rejection, occupied Local/Remote
  listener conflicts, remote-policy rejection, and Local/Remote/Dynamic traffic
  and cancellation through Apple OpenSSH;
- guardian completion through explicit pipe IPC plus listener and lock state.
  The portable harness has no `/proc`, `prctl`, or subreaper dependency.

An Actions result is automated evidence only. The first complete native result
for this change is [job 106943102778][native-arm64]: commit `319158b`, macOS
26.6.2 (25G83), `arm64`, Apple OpenSSH_10.3p1 with LibreSSL 3.3.6. All Rust and
Apple OpenSSH checks passed. This proves the current Apple Silicon runner, but
not the macOS 13 minimum or the shipped release artifact.

[native-arm64]: https://github.com/kshiva1126/tunnel-deck/actions/runs/35786079197/job/106943102778

| Architecture | Minimum/native OS | Automated Rust + Apple SSH | Release artifact smoke test |
| --- | --- | --- | --- |
| Apple Silicon (`aarch64`) | macOS 13 pending; 26.6.2 native | Pass (`319158b`) | Pending |
| Intel (`x86_64`) | macOS 13 | Pending native result | Pending |

## Human release-candidate checks

Run these on each architecture using the artifact intended for publication;
record the tester, date, exact OS, architecture, artifact checksum, and result.
Do not put SSH credentials or private configuration in the record.

- Install by the documented distribution method on a clean user account and
  launch `tdeck` without bypassing Gatekeeper globally.
- Exercise dashboard navigation, arrows and `j`/`k`, Enter, Space, `n`, `e`,
  delete confirmation, `/`, Tab, `?`, `q`, and Ctrl+C; confirm terminal state
  is restored after normal exit, signal exit, and a surfaced error.
- Discover a disposable SSH alias, create and operate Local, Remote, and
  Dynamic rules, confirm an occupied local port is not taken over, and confirm
  closing/reopening the TUI leaves a daemon-managed tunnel controllable.
- Use the explicit HTTP/HTTPS action and confirm the macOS `open` command opens
  the displayed URL in the selected browser without altering it.
- Stop all rules and the daemon, then confirm guardians, SSH children, sockets,
  attempt directories, and the inherited lock are cleaned up.

These checks are currently pending. Therefore neither architecture has a
recorded release-artifact execution result, and macOS support remains under
validation rather than release-verified.
