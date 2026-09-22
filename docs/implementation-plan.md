# Implementation plan

This document turns the product specification and architecture into an
implementation sequence. It defines the initial compatibility contracts before
code is written and keeps each milestone small enough to verify independently.

Execution is tracked in the [GitHub Issues roadmap](https://github.com/kshiva1126/tunnel-deck/issues/13).
Use its linked issues for current progress, dependencies, and completion checks.
Keep this document as the design/milestone reference and update both when scope
changes. Begin implementation with issue #1.

## Decisions for the initial implementation

Use the following defaults unless the repository owner explicitly changes
them before the affected milestone starts:

1. **Supervise the system OpenSSH client.** OpenSSH compatibility is more
   important to the MVP than an in-process Rust SSH transport.
2. **Start the daemon on demand.** A CLI or TUI client starts the per-user
   daemon when its socket is absent. A systemd user unit remains an optional
   distribution feature.
3. **Support non-interactive authentication only.** The MVP supports ssh-agent
   and key-based authentication that does not require a terminal prompt. It
   does not store, request, or forward passwords and key passphrases.
4. **Do not import another application's configuration.** Add importers later
   as explicit, separately tested migrations if users need them.
5. **Use UUIDs as stable rule identities.** Human-readable names remain unique
   and editable, but persisted references and IPC operations use rule IDs.
6. **Make the daemon the only configuration writer.** CLI and TUI clients
   perform mutations through IPC so concurrent writes cannot diverge.

The project uses MIT; see `LICENSE` and `CONTRIBUTING.md`. The accepted
[design decisions](design-decisions.md) specify SSH control, readiness, crash
cleanup, and defaults. Do not copy third-party code or assets until their
licenses have been verified and all required notices can be preserved.

## Version 1 compatibility contracts

These contracts should be documented in code and covered by fixtures before
the daemon or TUI depends on them.

### Persisted configuration

Store desired configuration as versioned TOML. Use an internally tagged enum
for forwarding-specific fields so fields that do not apply are omitted rather
than filled with placeholder values. A representative Local rule is:

```toml
schema_version = 1

[[rules]]
id = "018f6ba0-3be8-7c42-9f1f-61b99fc2e076"
name = "database"
ssh_host_alias = "production"
kind = "local"
bind_address = "127.0.0.1"
bind_port = 5433
destination_host = "127.0.0.1"
destination_port = 5432
auto_start = false
reconnect = false
```

The forwarding variants are:

- Local: bind address and port plus destination host and port.
- Remote: remote bind address and port plus destination host and port.
- Dynamic: bind address and port only.

Configuration writes must use a temporary file in the same directory, flush
the file, atomically rename it, and flush the parent directory where supported.
Before a migration, retain a recoverable copy of the previous configuration.
Tests must inject XDG paths and never read or write the developer's real home
directory.

Do not persist a PID and assume it still identifies a managed SSH process.
Runtime status belongs in daemon memory. If `state.json` becomes necessary for
recovery metadata, give it an independent schema version and never use it as
proof that a process is alive.

### IPC protocol

Use newline-delimited JSON over a per-user Unix socket for version 1. Every
request contains a protocol version, UUID request ID, operation, and payload.
Every response echoes the request ID and contains either a result or a
structured error. Event subscriptions use separate messages with a monotonically
increasing sequence number.

Protocol requirements:

- Maximum encoded message size: 1 MiB.
- Reject malformed, oversized, and unsupported-version messages before
  dispatch.
- Escape embedded newlines through normal JSON encoding; a literal newline is
  the frame delimiter.
- Use stable machine-readable error codes and human-readable diagnostics.
- Apply timeouts to client connections and request/response operations.
- Make start and stop operations idempotent.
- Close subscriptions cleanly when clients disconnect or lag beyond a bounded
  event buffer.
- Create the runtime directory with mode `0700` and the socket with mode
  `0600`; verify that an existing socket is owned by the current user.

The first request must negotiate or declare the protocol version. A mismatched
client and daemon must fail with a clear upgrade/restart diagnostic instead of
attempting partial compatibility.

### CLI surface

Validate this command tree during Milestone 0 and then treat it as a public
interface:

```text
tdeck                              Open the TUI
tdeck host list                    List discovered SSH hosts
tdeck host show <alias>            Show effective host settings
tdeck host test <alias>            Test non-interactive connectivity
tdeck forward list                 List forwarding rules
tdeck forward add                  Add a rule non-interactively
tdeck forward remove <rule>        Remove a stopped rule
tdeck forward start <rule>         Start a rule
tdeck forward stop <rule>          Stop a rule
tdeck status                       Show daemon and tunnel status
tdeck daemon run                   Run the internal daemon entry point
```

Commands intended for automation should support a stable JSON output mode.
Rule arguments may accept an exact unique name for convenience, but responses
and persisted references should always include the UUID.

The implemented Local CLI uses explicit flags for non-interactive creation and
supports global `--json` output. Rule start, stop, and removal accept an exact
name or UUID and always return the UUID. Exit statuses distinguish usage (2),
not found (4), conflict (5), service unavailable (6), and internal failure
(70). An occupied explicitly requested local port is a conflict; the CLI does
not choose a replacement or wait for input.

### OpenSSH command construction

Spawn OpenSSH directly with an argument vector and never through a shell.
Use one foreground private master per rule attempt, then add the forwarding
through `ssh -O forward`. Follow the exact two-stage construction in
[design decisions](design-decisions.md#one-private-openssh-master-per-rule).
Never combine `ClearAllForwardings=yes` with the requested `-L`, `-R`, or `-D`
in one invocation. Never reuse the user's control socket.

Do not add options that weaken host-key verification. Preserve the configured
host alias so OpenSSH continues to apply the user's `Host`, `Include`,
`ProxyJump`, identity, agent, and known-hosts behavior. Treat the SSH
configuration as user-controlled input and ensure logs do not expose sensitive
arguments or environment values.

Use the guardian, lease, process-group, and inherited-lock design in
[crash cleanup](design-decisions.md#daemon-crash-cleanup). Confirm forwarding
acceptance before reporting Active. Verify these semantics with real OpenSSH
before completing Milestone 2.

### Host discovery

Enumerate concrete aliases from the user's OpenSSH configuration, following
`Include` files with cycle protection and bounded traversal. Do not attempt to
turn wildcard-only `Host` patterns into concrete hosts. Resolve displayed
effective settings with `ssh -G <alias>` so TunnelDeck does not reimplement
OpenSSH precedence rules. Refreshing the host list must not require restarting
the daemon or TUI.

## Module boundaries

Begin with one Rust package and one `tdeck` binary:

```text
src/
├── main.rs
├── cli/
├── tui/
├── application/
├── domain/
│   ├── host.rs
│   ├── rule.rs
│   ├── status.rs
│   └── validation.rs
├── daemon/
│   ├── lifecycle.rs
│   ├── manager.rs
│   ├── reconnect.rs
│   └── process.rs
├── ipc/
├── config/
├── platform/
└── logging/
```

Keep domain types independent of Ratatui, Clap, Tokio process APIs, and wire
serialization where practical. UI code must call application operations
through the same IPC service as the CLI and must never own SSH children.

## Milestone 0 — scaffold and freeze contracts

Implementation status: complete on the Issue #1 branch pending review and CI.
The scaffold uses Rust 1.85 / edition 2024. Initial platform baselines are Linux
kernel 5.15 with glibc 2.35 and macOS 13. These baselines may only be widened
after native compatibility checks; release artifacts are not yet available.

Tasks:

- Initialize a Rust binary package named `tunnel-deck` with binary name
  `tdeck`, and commit `Cargo.lock`.
- Add the module skeleton, typed top-level errors, and explicit process exit
  codes.
- Implement the Clap command definitions without claiming unfinished tunnel
  behavior.
- Add serde types and fixtures for configuration and IPC version 1.
- Set Cargo package license to `MIT`; retain the existing license and
  contribution policy.
- Select minimum Linux/macOS versions and a compatible Rust toolchain before
  freezing dependencies. Record macOS deployment targets explicitly.
- Add Linux and macOS CI jobs that run formatting, Clippy, and tests.

Exit criteria:

- `tdeck --help` and `tdeck --version` work.
- Configuration and IPC fixtures serialize to the documented version 1 shape.
- Checks pass in a clean checkout.
- Help output clearly marks or omits behavior that is not implemented.

## Milestone 1 — domain, configuration, and host discovery

Tasks:

- Implement Local, Remote, and Dynamic rule types with stable IDs.
- Validate names, addresses, port ranges, required fields, and duplicate local
  listeners with structured errors.
- Implement the named runtime-state transitions independently of processes.
- Resolve Linux/macOS paths and XDG overrides with restrictive permissions.
- Load and atomically save versioned TOML with migration infrastructure.
- Discover explicit SSH host aliases and resolve their effective settings via
  `ssh -G`.

Exit criteria:

- Invalid and conflicting rules fail before process construction.
- All rule variants round-trip without adding irrelevant fields.
- Configuration tests use temporary directories only.
- Included SSH configurations, cycles, missing files, and wildcard patterns
  have deterministic tests.

## Milestone 2 — daemon and Local forwarding vertical slice

Implementation status: daemon IPC and guardian-owned Local forwarding are
implemented on the GH-5 branch pending review and native CI. Fake-SSH lifecycle
tests cover acceptance, early failure, bounded output, concurrent stop,
lease-EOF cleanup, and forced termination. The repository's isolated OpenSSH
probe supplies prior Linux protocol evidence; native execution of the Rust
path with real OpenSSH and macOS lifecycle validation remain review conditions.

Tasks:

- Implement the Unix-socket server, client, framing limits, request dispatch,
  and event subscriptions.
- Add race-safe on-demand daemon startup and enforce one daemon per user.
- Locate and validate the OpenSSH executable.
- Build the private master and Local control-forward arguments without a shell.
- Verify the two-stage OpenSSH design and guardian crash cleanup in an isolated
  integration environment, including Remote and Dynamic protocol probes.
- Run native lifecycle and control-forward integration checks on Linux and
  macOS, including Apple's system SSH. Replace Linux-specific probe harness
  assumptions; cross-compilation alone does not validate process behavior.
- Supervise the SSH process group, detect early listener failure, retain a
  bounded stderr diagnostic, and implement graceful/forced shutdown.
- Expose create, list, start, stop, status, and delete through the CLI.

Exit criteria:

- The CLI can persist, start, inspect, stop, and remove a Local rule.
- A tunnel remains active after the invoking CLI process and TUI client exit.
- Repeated start and stop requests are safe and deterministic.
- A fake SSH executable covers success, nonzero exit, delayed exit, large
  stderr, signal handling, and forced termination.
- IPC tests cover malformed frames, limits, timeouts, disconnects, permissions,
  and version mismatch.

## Milestone 3 — usable TUI

Implementation status: the host selection to Local port workflow, dashboard,
details, help, explicit browser actions, daemon event refresh, and terminal
restoration are implemented for GH-7. Native macOS terminal/signal/browser
validation and remote CI remain post-publication review conditions.

Tasks:

- Add terminal setup and restoration on normal exit, error, panic, and signal.
- Implement the dashboard, host browser, type-aware rule form, confirmation
  dialog, diagnostics view, settings, and contextual help.
- Prioritize host selection -> remote port entry -> local address display and
  explicit browser opening. Offer a user-selected alternative for occupied
  local ports, retaining startup-time conflict detection.
- Subscribe to daemon events rather than polling on every rendered frame.
- Preserve selection across updates and provide a useful small-terminal
  fallback.
- Ensure destructive operations require confirmation.

Exit criteria:

- A user can discover a host, create a Local rule, and run it without editing
  files.
- Keyboard operations are discoverable and terminal state is always restored.
- Rendering and form tests cover normal, narrow, short, and empty states.
- Closing the TUI does not stop daemon-managed tunnels.

## Milestone 4 — forwarding parity and recovery

Implementation status: Remote and Dynamic forwarding, daemon-owned auto-start
and reconnect scheduling, redacted rule diagnostics, uptime/reconnect counters,
and a bounded rotation primitive are implemented in GH-8. Versioned settings
persistence and UI/CLI settings are tracked in GH-42; connecting the configured
log level and daemon events to rotation is tracked in GH-43. The parent issue
remains open until those follow-ups are complete.

GH-42 implements the settings follow-up with TOML schema v2, an explicit
backup-first v1 migration, daemon-owned IPC get/update operations, CLI JSON and
human output, and a TUI settings page. Theme and log-level persistence are
contracts for their consumers; runtime log filtering remains GH-43.

Tasks:

- Add Remote and Dynamic OpenSSH argument construction and controls.
- Add capped exponential reconnect with jitter and cancellation.
- Start configured `auto_start` rules when the daemon becomes ready.
- Reconcile daemon restarts without adopting or duplicating unmanaged
  processes.
- Add local port-availability warnings and classify common OpenSSH failures
  without relying on a single locale-specific stderr string.

Exit criteria:

- Local, Remote, and Dynamic rules pass isolated end-to-end tests.
- Recovery cannot launch duplicate children for one rule.
- Manual stop cancels pending reconnect immediately.
- Authentication, host-key, listener, remote rejection, and network failures
  remain distinguishable without leaking credentials.

## Milestone 5 — distribution and hardening

Tasks:

- Produce Linux `x86_64`/`aarch64` and macOS Intel/Apple Silicon artifacts and
  checksums. Verify linkage and minimum OS versions selected at Milestone 0.
- Define and test macOS signing/notarization and installation handling before
  public distribution; do not instruct users to disable Gatekeeper globally.
- Add shell completions, man pages, and upgrade documentation.
- Optionally add a systemd user unit without making it mandatory.
- Audit dependency licenses and preserve required third-party notices.
- Review IPC permissions, atomic writes, command construction, logging,
  process signaling, and denial-of-service bounds.

Exit criteria:

- A fresh user on either OS can install its binary and complete every MVP acceptance
  criterion from the product specification.
- Installation and upgrade are documented and reproducible.
- Release artifacts pass native smoke tests on all four OS/architecture targets.

## Test matrix and completion checks

Use deterministic unit and integration tests by default:

- Domain: validation, conflicts, and every permitted or rejected state
  transition.
- Configuration: round-trip, unknown version, migration, interrupted write,
  permissions, and XDG fallback behavior.
- Protocol: framing, malformed JSON, size limit, request correlation, event
  ordering, lagging subscribers, and version mismatch.
- Process: fake `ssh` behavior, exact argument construction, bounded output,
  early failure, unexpected exit, and signal escalation.
- TUI: buffer or snapshot tests at several terminal sizes and input-driven form
  tests.
- Integration: daemon and clients in an isolated temporary runtime directory.
- Optional end-to-end: a containerized SSH server for all forwarding types,
  independent of the developer's personal SSH configuration.

Before reporting any implementation milestone complete, run:

```text
cargo fmt --check
cargo clippy --all-targets --all-features
cargo test
```

Document any additional platform-dependent end-to-end checks separately; they
must not make the default test suite depend on network access or a real SSH
account.

## First implementation session

1. Apply the accepted design decisions and MIT license metadata.
2. Implement only Milestone 0.
3. Review the committed configuration and IPC fixtures before building the
   daemon against them.
4. Run all completion checks.
5. Report files changed, verification results, and remaining decisions without
   advertising unimplemented SSH behavior.
