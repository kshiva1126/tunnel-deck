# Implementation plan

## Decisions to confirm first

The next implementation agent should confirm these with the user if they would
materially change the initial code structure:

1. **OpenSSH process supervision or native SSH?** The current recommendation is
   OpenSSH for the MVP because compatibility is more important than a
   pure-Rust transport stack.
2. **Daemon startup model.** Start on demand from `tdeck`, with optional user
   systemd integration later, or install a systemd user unit immediately? The
   current recommendation is on-demand first.
3. **Interactive authentication.** Agent/key-based authentication is simplest
   for a background daemon. Password and passphrase prompting requires a secure
   request/response flow and should not be improvised.
4. **Configuration compatibility.** No compatibility with another application's
   data format is required unless the user explicitly requests an importer.

## Milestone 0 — scaffold and contracts

- Initialize a Rust binary package named `tunnel-deck`.
- Configure the executable name as `tdeck`.
- Add formatting, Clippy, tests, and a minimal Linux CI workflow.
- Establish module boundaries and typed error handling.
- Write protocol and persisted-config schemas before implementing the daemon.
- Add license and contribution policy chosen by the repository owner.

Exit criteria:

- `tdeck --help` and `tdeck --version` work.
- Checks pass in a clean checkout.
- No tunnel behavior is falsely advertised yet.

## Milestone 1 — testable domain and configuration

- Define forwarding rule types with per-type fields.
- Implement validation and name/ID rules.
- Implement XDG path resolution.
- Load and atomically save versioned TOML configuration.
- Add migration infrastructure even if only schema version 1 exists.

Exit criteria:

- Invalid and conflicting rules have structured errors.
- Configuration round trips without losing data.
- Tests do not touch real user directories.

## Milestone 2 — daemon and local forwarding

- Implement per-user daemon lifecycle and Unix-socket IPC.
- Supervise OpenSSH without a shell.
- Implement Local forwarding and `ExitOnForwardFailure` handling.
- Publish state changes and bounded stderr diagnostics.
- Implement graceful and forced shutdown behavior.

Exit criteria:

- CLI can start, inspect, and stop a Local rule.
- Tunnels remain active after the CLI process exits.
- Tests use a fake SSH executable; optional real-SSH tests are isolated.

## Milestone 3 — usable TUI

- Add terminal lifecycle and panic/error restoration.
- Implement dashboard, host browser, rule form, confirmation dialog, logs, and
  contextual help.
- Subscribe to daemon events rather than polling every frame.
- Support small-terminal fallback behavior.

Exit criteria:

- A user can create and run a Local rule without editing files.
- Keyboard operations are discoverable.
- Rendering and form behavior have automated tests.

## Milestone 4 — forwarding parity and recovery

- Add Remote and Dynamic forwarding.
- Add capped exponential reconnect with jitter.
- Add auto-start and daemon-restart reconciliation.
- Add port-conflict detection and clearer SSH diagnostics.

Exit criteria:

- All three forwarding types pass isolated end-to-end tests.
- Recovery never launches duplicate unmanaged processes.

## Milestone 5 — distribution and hardening

- Produce Linux `x86_64` and `aarch64` artifacts and checksums.
- Decide whether binaries dynamically or statically link platform libraries.
- Add shell completions and man pages.
- Optionally add a systemd user service and packaging.
- Conduct security review of IPC permissions, command construction, logs, and
  process signaling.

Exit criteria:

- Installation and upgrade are documented and reproducible.
- A release artifact works on the documented minimum Linux environment.

## First-session checklist for the next Codex run

1. Read all repository documentation.
2. Inspect the local Rust toolchain and Git status.
3. Ask only about unresolved decisions that block the selected milestone.
4. Create a small plan and implement Milestone 0 without adding speculative
   SSH behavior.
5. Run format, Clippy, and tests.
6. Summarize files changed, verification results, and remaining decisions in
   Japanese.
