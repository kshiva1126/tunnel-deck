# Product specification

## Problem

SSH port forwarding is powerful but its command-line syntax, background
process lifecycle, and failure modes are difficult to manage repeatedly.
TunnelDeck should provide one place to create, run, observe, and troubleshoot
tunnels.

## Product principles

1. **TUI first, CLI always available.** Interactive users should not need to
   edit configuration manually; scripts should not need to drive a TUI.
2. **Use familiar SSH behavior.** Respect the user's OpenSSH configuration,
   agent, known-hosts policy, and jump-host setup wherever practical.
3. **Safe defaults.** Host-key verification stays enabled, destructive actions
   require confirmation, and secrets never enter persisted configuration.
4. **Visible state.** A tunnel must clearly show whether it is starting,
   active, reconnecting, stopped, or failed, with an actionable reason.
5. **Keyboard efficient.** Common actions should require few keystrokes and
   remain discoverable through contextual help.

## Target environment

The initial release targets are Linux on `x86_64` and `aarch64`, and macOS on
Apple Silicon (`aarch64`) and Intel (`x86_64`). Both operating systems must meet
the same MVP acceptance criteria. Windows is not an MVP target.

TunnelDeck runs on the local computer where the browser runs. Its SSH server
may run a different OS, provided it supports the required forwarding behavior.
No TunnelDeck installation on the SSH server is required.

## Primary workflow: remote development ports

Choose an SSH config host, enter the remote development-server port (for example
3000), and start Local forwarding to remote `127.0.0.1:3000`. Default the local
listener to `127.0.0.1:3000`. Show the actual local address, with an explicit
HTTP/HTTPS browser-open action for web services; not every TCP service is HTTP.
Advanced forwarding options remain available separately.

If the local port is occupied, propose an available alternative such as 3001
and let the user select it. Change only the local port, save that selection,
and show the resulting address. Never terminate the existing listener or
silently change ports on restart. Candidate availability is advisory: if a
process claims it before SSH binds, report the failure and offer another choice.
CLI requests for a specific port fail with a structured conflict error and do
not prompt or silently substitute ports. Automatic remote port discovery is
outside the MVP.

## MVP capabilities

### SSH hosts

- Read hosts from `~/.ssh/config`, including `Include` behavior where feasible.
- Show alias, hostname, port, user, identity selection, and proxy/jump settings.
- Refresh the host list without restarting the application.
- Provide a connection test with a useful diagnostic message.
- Reuse existing `~/.ssh/config` aliases without re-entering connection details.
  Delegate user, identity, jump-host, and host-key settings to OpenSSH. TunnelDeck
  does not edit this file. Existing forwarding directives are suppressed for
  managed connections so they do not start additional untracked listeners.
- After the MVP, offer explicit import of existing `LocalForward`,
  `RemoteForward`, and `DynamicForward` settings into editable TunnelDeck rules,
  with preview and conflict validation before saving. This is separate from
  importing another application's configuration.

### Forwarding rules

- Local forwarding (`-L`).
- Remote forwarding (`-R`).
- Dynamic SOCKS forwarding (`-D`).
- Create, edit, duplicate, delete, start, and stop rules from the TUI.
- Validate names, addresses, ports, duplicate listeners, and required fields.
- Allow an optional auto-start flag.

A rule should contain at least:

```text
id
name
ssh_host_alias
kind = local | remote | dynamic
bind_address
bind_port
destination_host
destination_port
auto_start
reconnect
```

Fields that do not apply to a forwarding type should be omitted from its
serialized representation rather than filled with misleading values.

### Lifecycle and recovery

- Keep tunnels running when the TUI exits, using a per-user background daemon.
- Detect unexpected SSH process termination.
- Optional reconnect with capped exponential backoff.
- Restore configured auto-start rules when the daemon starts.
- Provide graceful stop first and forced termination only as a fallback.
- New rules default to auto-start and reconnect disabled. Active means forwarding
  setup was accepted, not that the destination service is healthy. See
  [design decisions](design-decisions.md) for exact behavior and crash boundaries.

### Observability

- Display state, PID or internal task identifier, uptime, reconnect count, and
  last error.
- Stream structured daemon events to the TUI.
- Keep rotating application logs without recording credentials.
- Treat accurate byte counters as post-MVP unless the selected forwarding
  implementation can expose them reliably.

### Configuration

On Linux, use XDG paths by default:

```text
$XDG_CONFIG_HOME/tunnel-deck/config.toml
$XDG_STATE_HOME/tunnel-deck/tunnel-deck.log
$XDG_RUNTIME_DIR/tunnel-deck/tdeck.sock
```

On macOS, default to `~/Library/Application Support/TunnelDeck/config.toml`
and `~/Library/Logs/TunnelDeck/tunnel-deck.log`. Use a private short runtime
directory as specified in [platform decisions](design-decisions.md#platform-support).
Explicit XDG config/state overrides are supported on either OS.

Provide documented fallbacks when an XDG variable is absent. Runtime sockets
must be accessible only by the current user. Configuration writes should be
atomic and should preserve a recoverable previous copy when migration occurs.
Version 1 does not persist runtime process state. The accepted design decisions
define path fallbacks and permissions.

## TUI outline

### Dashboard

- Summary counts and daemon health.
- Table of forwarding rules with state and uptime.
- One-key start/stop action and clear status colors plus text labels.

### Hosts

- Searchable host list.
- Host detail pane.
- Refresh and connection-test actions.

### Rule editor

- Type-aware form that only shows relevant fields.
- Inline validation before saving.
- Port availability warning for local listeners.

### Logs and diagnostics

- Filter by rule and severity.
- Copyable error details.
- Clear separation between a configuration error, authentication failure,
  host-key failure, network failure, and remote-forward rejection.

### Settings and help

- Reconnection settings, theme, log level, and startup behavior.
- Contextual key hints and a full keybinding overlay.

## Suggested key model

```text
j/k or arrows   Move selection
Enter           Open details / confirm
Space           Start or stop selected rule
n               New rule
Esc             Return from the host list opened with n; close an open form or import panel first
e               Edit
d               Delete with confirmation
/               Search or command palette
Tab             Change pane
?               Contextual help
q               Quit TUI, leaving daemon-managed tunnels running
Ctrl+C          Exit safely and restore terminal
```

On terminals that report mouse input, clicking a top tab changes the page.
Clicking a dashboard rule row or host row selects it, and the wheel moves the
selection in that list. A click does not start, stop, remove, edit, connect, or
open a browser. Keyboard actions remain available when mouse input is absent.
Popups and drag gestures are outside this mouse interaction scope.

## Explicit non-goals for the first release

- A graphical desktop UI.
- Multi-user or system-wide daemon operation.
- Cloud synchronization.
- Storing SSH passwords or private keys.
- Full OpenSSH configuration reimplementation if delegating to the system
  `ssh` executable provides better compatibility.
- Windows support.

## MVP acceptance criteria

1. A fresh Linux or macOS user can install the locked source with Cargo and
   launch `tdeck`.
2. The TUI discovers a host from the user's SSH config.
3. The user can create and persist one rule without editing a file.
4. Local, remote, and dynamic rules can be started and stopped.
5. A running tunnel survives TUI exit and remains controllable after reopening.
6. Authentication and host-key failures are surfaced without leaking secrets.
7. Invalid ports and conflicting local listeners are rejected before launch.
8. The CLI can list and control the same rules managed by the TUI.
9. Unit tests cover configuration, validation, state transitions, and command
   construction; integration tests cover daemon IPC and process lifecycle.
10. A user can forward a remote development port, choose an alternative when
    the local port is occupied, and open the resulting local web address.
