# Architecture

## Recommended shape

Build one Rust package that produces a single `tdeck` executable with multiple
entry modes:

```text
                    Unix socket
TUI ─────────────── JSON messages ──────────────┐
CLI ─────────────── JSON messages ──────────────┤
                                                 ▼
                                      per-user daemon
                                      ├── rule manager
                                      ├── SSH process manager
                                      ├── reconnect scheduler
                                      ├── config/state store
                                      └── event/log publisher
                                                 │
                                                 ▼
                                             OpenSSH
```

The TUI must be a client of the same application service used by the CLI. UI
code should never own tunnel processes directly; otherwise tunnels would stop
when the terminal closes and behavior would diverge between CLI and TUI.

## Initial SSH strategy

Supervise the system OpenSSH client. Each rule attempt owns a foreground
private master; add the requested forwarding through its control socket after
authentication. See [accepted design decisions](design-decisions.md) for exact
arguments, readiness, guardian ownership, and daemon-crash cleanup.

The actual argument list must be constructed without invoking a shell. Passing
the configured host alias to OpenSSH preserves compatibility with the user's
`~/.ssh/config`, `ProxyJump`, agent, and known-hosts behavior. Do not translate
the config into a partial internal representation and then accidentally ignore
important OpenSSH options.

Important implementation details:

- Locate and validate the `ssh` executable at startup.
- Never concatenate user values into a shell command.
- Capture stderr for diagnostics with bounded buffering.
- Report Active only after the control forwarding request succeeds; do not
  imply that the destination service is reachable.
- Define how interactive authentication works before claiming password support.
  A background process without a controlling terminal cannot safely prompt in
  the ordinary way.
- Gracefully terminate the child and its process group, then apply a bounded
  forced shutdown if required.
- Record enough process identity to avoid signaling an unrelated process after
  PID reuse.

A native Rust SSH engine can be evaluated later if it provides a concrete
benefit such as accurate traffic counters or eliminating the external binary.
It should not be selected merely because the application is written in Rust;
OpenSSH compatibility is a product requirement.

## Proposed Rust components

These are recommendations, not locked dependency versions:

- `ratatui` and `crossterm`: terminal rendering and input.
- `tokio`: asynchronous runtime, process supervision, IPC, timers, and signals.
- `clap`: command-line parsing.
- `serde`, `toml`, and `serde_json`: configuration and IPC serialization.
- `tracing` and `tracing-subscriber`: structured diagnostics.
- `thiserror` plus an application-level diagnostic layer: typed failures.
- `directories` or a small XDG-specific module: platform paths.
- `uuid`: stable rule and request identifiers if human names are editable.

Pin versions only when scaffolding begins, then commit `Cargo.lock` because this
is an application rather than a library.

## Suggested module boundaries

```text
src/
├── main.rs                 Mode selection and process exit status
├── cli/                    Clap definitions and non-interactive output
├── tui/                    Rendering, input, forms, and UI state
├── application/            Use cases shared by IPC handlers and tests
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
├── ipc/                    Versioned request/response/event protocol
├── config/                 XDG paths, TOML, migration, atomic persistence
├── platform/               Linux/macOS paths, process operations, browser launch
└── logging/
```

Keep domain types free of Ratatui, Clap, Tokio process, and serialization
details where reasonable. This allows state-machine and validation tests to run
without terminals, sockets, or real SSH connections.

## IPC

Use a per-user Unix socket under the shared platform path resolver's runtime
directory (see [platform support](design-decisions.md#platform-support)). The protocol
should be explicitly versioned from its first revision.

Requirements:

- Request IDs and structured error codes.
- Bounded message size.
- Separate request/response operations from event subscriptions.
- Peer access restricted by socket directory and file permissions.
- Timeouts and cancellation for client calls.
- Compatibility behavior for a client/daemon version mismatch.
- Idempotent start and stop requests.

Newline-delimited JSON is sufficient for the MVP if framing, maximum length,
and embedded newline behavior are defined. A binary format is unnecessary
until measurement demonstrates a problem.

### Implemented daemon IPC foundation

The client now starts `tdeck daemon run` on demand and waits for its private
socket. The daemon holds a nonblocking `flock` on the permanent `daemon.lock`
before removing a stale socket, so concurrent launchers cannot create two
owners. The runtime directory and socket are validated as current-user-owned
0700/0600 objects; unsafe objects are rejected rather than repaired.

Each connection uses bounded newline JSON with a five-second I/O timeout.
Requests carry UUID v4 IDs and version 1; responses echo the ID or return a
stable structured error. The daemon owns configuration mutation and its
in-memory start-request intent. Start and stop are idempotent at this boundary;
actual OpenSSH process ownership remains in the later forwarding slice.
Subscriptions receive monotonically numbered events through a 64-event queue;
a subscriber is removed when its queue fills or its socket disconnects.

## State model

Model rule runtime state explicitly using the transitions and deadlines in
[runtime state](design-decisions.md#runtime-state-and-deadlines), including stop
during Starting and cancellation of stale attempt events.

Every transition should be caused by a named event and be unit-testable. Avoid
deriving state solely from whether a PID exists.

Persist desired configuration and minimal recovery metadata, not an assumption
that a recorded process is still valid. On daemon restart, reconcile actual
process state and re-establish only rules whose policy requests restoration.

## Security boundaries

- Keep host-key verification controlled by OpenSSH and never inject
  `StrictHostKeyChecking=no` as a convenience.
- Do not store passwords or key passphrases.
- Redact credentials, environment values, and sensitive command arguments from
  diagnostics.
- Use restrictive permissions for configuration, state, logs, and IPC.
- Validate port ranges and addresses before spawning a child.
- Treat SSH config and forwarding destinations as user-controlled input.
- Avoid a shell for process execution.

## Testing strategy

- Domain tests: rule validation and state transitions.
- Snapshot or buffer tests: TUI rendering at several terminal sizes.
- Protocol tests: framing, malformed messages, version mismatch, and limits.
- Process tests: use a fake `ssh` executable with deterministic stdout, stderr,
  exit timing, and signal behavior.
- IPC integration tests: daemon in a temporary runtime directory.
- Native Linux and macOS process tests: descriptor inheritance, guardian
  cleanup, lock ownership, signals, and terminal restoration. The existing
  Linux-only experiment is not evidence of macOS support.
- Optional end-to-end tests: containerized SSH server for all three forwarding
  types; these should not depend on a developer's personal SSH configuration.
