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

The initial supported platform is Linux on `x86_64` and `aarch64`. macOS may be
added later. Windows is not an MVP target because the planned daemon IPC and
process management are Unix-oriented.

## MVP capabilities

### SSH hosts

- Read hosts from `~/.ssh/config`, including `Include` behavior where feasible.
- Show alias, hostname, port, user, identity selection, and proxy/jump settings.
- Refresh the host list without restarting the application.
- Provide a connection test with a useful diagnostic message.

### Forwarding rules

- Local forwarding (`-L`).
- Remote forwarding (`-R`).
- Dynamic SOCKS forwarding (`-D`).
- Create, edit, duplicate, delete, start, and stop rules from the TUI.
- Validate names, addresses, ports, duplicate listeners, and required fields.
- Allow an optional auto-start flag.

A rule should contain at least:

```text
name
ssh_host_alias
type = local | remote | dynamic
bind_address
local_port
remote_host
remote_port
auto_start
```

Fields that do not apply to a forwarding type should be omitted from its
serialized representation rather than filled with misleading values.

### Lifecycle and recovery

- Keep tunnels running when the TUI exits, using a per-user background daemon.
- Detect unexpected SSH process termination.
- Optional reconnect with capped exponential backoff.
- Restore configured auto-start rules when the daemon starts.
- Provide graceful stop first and forced termination only as a fallback.

### Observability

- Display state, PID or internal task identifier, uptime, reconnect count, and
  last error.
- Stream structured daemon events to the TUI.
- Keep rotating application logs without recording credentials.
- Treat accurate byte counters as post-MVP unless the selected forwarding
  implementation can expose them reliably.

### Configuration

Use XDG paths by default:

```text
$XDG_CONFIG_HOME/tunnel-deck/config.toml
$XDG_STATE_HOME/tunnel-deck/state.json
$XDG_STATE_HOME/tunnel-deck/tunnel-deck.log
$XDG_RUNTIME_DIR/tunnel-deck/tdeck.sock
```

Provide documented fallbacks when an XDG variable is absent. Runtime sockets
must be accessible only by the current user. Configuration writes should be
atomic and should preserve a recoverable previous copy when migration occurs.

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
e               Edit
d               Delete with confirmation
/               Search or command palette
Tab             Change pane
?               Contextual help
q               Quit TUI, leaving daemon-managed tunnels running
Ctrl+C          Exit safely and restore terminal
```

## Explicit non-goals for the first release

- A graphical desktop UI.
- Multi-user or system-wide daemon operation.
- Cloud synchronization.
- Storing SSH passwords or private keys.
- Full OpenSSH configuration reimplementation if delegating to the system
  `ssh` executable provides better compatibility.
- Windows support.

## MVP acceptance criteria

1. A fresh Linux user can install one binary and launch `tdeck`.
2. The TUI discovers a host from the user's SSH config.
3. The user can create and persist one rule without editing a file.
4. Local, remote, and dynamic rules can be started and stopped.
5. A running tunnel survives TUI exit and remains controllable after reopening.
6. Authentication and host-key failures are surfaced without leaking secrets.
7. Invalid ports and conflicting local listeners are rejected before launch.
8. The CLI can list and control the same rules managed by the TUI.
9. Unit tests cover configuration, validation, state transitions, and command
   construction; integration tests cover daemon IPC and process lifecycle.

