# Initial design decisions

Accepted on 2026-09-22. These decisions supersede earlier conceptual SSH
invocations. They define implementation targets. The initial isolated
[OpenSSH probe](openssh-probe.md) passed; its results and limits do not establish
production readiness.

## License and contribution policy

Use the MIT license, SPDX identifier `MIT`. Contributions use the same license;
no CLA is required. Preserve third-party license and attribution requirements.
See [LICENSE](../LICENSE) and [CONTRIBUTING.md](../CONTRIBUTING.md).

## Platform support

Linux and macOS are initial release targets with the same feature set. Build
separate binaries for Linux x86_64/aarch64 and macOS x86_64/aarch64. Milestone 0
sets Rust 1.85 / edition 2024, Linux kernel 5.15 with glibc 2.35, and macOS 13
as the initial minimums. CI must compile with Rust 1.85. A deployment target or
successful cross-build does not replace native runtime checks.

Keep domain, TOML, IPC messages, CLI, and TUI common. Isolate path resolution,
process-group operations, descriptor inheritance, and browser launching behind
a small `platform` module. Use Unix sockets, socketpair leases, and guardians
on both systems. Do not depend on `/proc`, `prctl`, pidfds, or cgroups for the
baseline implementation. Apple documents inherited `flock` references in its
[flock manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/flock.2.html);
the complete cleanup and lock-lifetime behavior still needs macOS testing.
Guardians close inherited lock descriptors without explicit `LOCK_UN`, which
would unlock the shared lock for other holders too.

On-demand daemon launch is the default on both systems. Neither systemd nor
launchd registration is required. Optional login-start integration comes later.
Test macOS with Apple's `/usr/bin/ssh`; a separately installed OpenSSH may be
supported additionally, but must not become an undocumented prerequisite.

Path choices (application decisions, following macOS Library conventions in
[Apple's file-system guide](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/FileSystemOverview/FileSystemOverview.html)):

| Purpose | Linux default | macOS default |
| --- | --- | --- |
| Configuration | `~/.config/tunnel-deck/config.toml` | `~/Library/Application Support/TunnelDeck/config.toml` |
| Log | `~/.local/state/tunnel-deck/tunnel-deck.log` | `~/Library/Logs/TunnelDeck/tunnel-deck.log` |
| Runtime without XDG override | `/tmp/tunnel-deck-<uid>` | `/private/tmp/tunnel-deck-<uid>` |

On both OSes, an explicit absolute `XDG_CONFIG_HOME` or `XDG_STATE_HOME` uses
the existing `tunnel-deck` subdirectory convention. Prefer a valid absolute,
owned `XDG_RUNTIME_DIR` plus `tunnel-deck`; otherwise use the OS runtime default.
Keep runtime directories mode 0700 and files mode 0600, reject unsafe ownership
or symlinks in app-created paths, and check socket path byte lengths against
the actual target OS limit. All clients and guardians use the same resolver.
Never unlink an active runtime lock; temporary-directory cleanup and stale
sockets must not create a second daemon while the old daemon/guardians live.

The existing Python probe is Linux-only: its subreaper and `/proc` checks are
test-harness facilities, not portable implementation requirements. A macOS
test harness must verify guardian completion through explicit IPC and process
ownership without copying those Linux-only assumptions. Native macOS lifecycle
and real-SSH validation are required before claiming macOS support.

## One private OpenSSH master per rule

Use a foreground OpenSSH master with a fresh control socket for each start
attempt. Never reuse the user's master or share a master between rules. This
keeps stop, reconnect, and failure ownership local to one rule.

The master reads the user's SSH configuration and receives the original alias:

```text
ssh -N -T -n
    -o BatchMode=yes
    -o ClearAllForwardings=yes
    -o ControlMaster=yes
    -o ControlPersist=no
    -o ForkAfterAuthentication=no
    -o ExitOnForwardFailure=yes
    -o ServerAliveInterval=15
    -o ServerAliveCountMax=3
    -S <private-attempt-socket>
    <host-alias>
```

Do not supply `-L`, `-R`, or `-D` to this invocation. After a successful
control check, add exactly one forwarding through that socket:

```text
ssh -F /dev/null -S <private-attempt-socket> -O check <host-alias>
ssh -F /dev/null -S <private-attempt-socket> -O forward
    -o ClearAllForwardings=no -o ExitOnForwardFailure=yes
    -L <forward-spec> <host-alias>
```

Use `-R` or `-D` instead for the other variants. Control clients intentionally
read no configuration: authentication, routing, and host-key verification were
already applied by the master. A failed control request is an error; never
fall back to an independent connection. All invocations use argument vectors,
validated aliases that cannot be options, and bracketed IPv6 forwarding fields.

Each socket lives in a fresh, short, mode-0700 attempt directory under the
private runtime directory. Check Unix socket path-length limits before spawning.
Never connect to a pre-existing attempt socket. The master's standard input is
null, and diagnostics are bounded. Do not weaken host-key policy; unknown hosts
requiring confirmation need the user to establish trust outside TunnelDeck.

This replaces the earlier proposal to disable all multiplexing with
`ControlPath=none`: TunnelDeck now deliberately owns a private master.
The OpenSSH manuals document that
[ClearAllForwardings](https://man.openbsd.org/ssh_config#ClearAllForwardings)
also clears command-line forwards, and that
[control operations](https://man.openbsd.org/ssh#O) support checks and forwarding
requests. The two-stage design must be verified against real OpenSSH before
Milestone 2 is considered complete.

## Runtime state and deadlines

`Active` means the private master has accepted the requested forwarding and
has not subsequently been observed to exit. It does not guarantee destination
service availability or instantaneous network health. In particular, Local
and Dynamic forwarding can be established while their destinations are down.
Remote server policy may affect the actual remote bind address; display the
requested address without claiming it was independently verified.

- Start accepted: `Stopped` or `Failed` -> `Starting`.
- Control forwarding request succeeds, with no observed master exit:
  `Starting` -> `Active`.
- Startup fails or exceeds 30 seconds: clean up the entire attempt before
  entering `Failed`, or `Reconnecting` if retryable and enabled.
- Unexpected exit: `Active` -> `Reconnecting` or `Failed`.
- Retry timer fires: `Reconnecting` -> `Starting`.
- Manual stop from `Starting`, `Active`, `Reconnecting`, or `Failed`:
  `Stopping` -> `Stopped`, after cancellation and cleanup.

Use attempt IDs to discard late success/events after stop or retry. Individual
control calls have a 5-second timeout, within the overall startup deadline.
Process existence and a fixed sleep are never sufficient readiness checks.

Default new rules to `auto_start=false` and `reconnect=false`. Opt-in reconnect
uses full jitter from zero to `min(60 seconds, 2^attempt seconds)`, starting at
attempt zero. Reset the attempt counter after 60 seconds active. Retry transport
loss and timeouts; configuration errors, authentication/host-key failures,
forwarding rejection, and unclassified startup failures require user action.
Diagnostics may classify known messages as hints, but must retain an `unknown`
category rather than guess a security-related cause from a generic exit code.

## Daemon crash cleanup

Use a small internal guardian process per rule attempt, launched from the same
`tdeck` binary. The guardian owns and waits for the SSH children; the daemon owns
policy, persistence, and reconnect scheduling. Communication uses a private
socketpair. Only the daemon retains its endpoint; clients and SSH children must
not inherit it. EOF tells the guardian to stop the attempt even after daemon
SIGKILL. Guardians never reconnect and have no public IPC socket.

The guardian starts SSH in a separate process group. Cleanup sends SIGTERM,
waits at most 5 seconds, then sends SIGKILL if necessary. Keep the group leader
unreaped until group signaling finishes to prevent numeric group-ID reuse;
then reap children and remove the attempt directory. Do not adopt or signal
processes using persisted PIDs. User programs launched by SSH configuration
that deliberately detach from the group are outside this cleanup guarantee.

Hold a daemon-lifetime `flock` on a permanent private runtime lock file. Pass
the same locked open file description to every guardian. Guardians close it
only after cleanup; SSH children must not inherit it. Never unlink the lock
file. A replacement daemon cannot start tunnels until all old guardians have
finished. Wait up to 15 seconds, then report cleanup still in progress rather
than bypassing the lock. This uses the inherited-lock semantics documented in
[flock(2)](https://man7.org/linux/man-pages/man2/flock.2.html).

The guarantee covers daemon failure while guardians and the kernel remain
operational. Simultaneous guardian failure is not solved by a process group;
stronger containment with cgroups is a Linux-only future hardening option.
Verify daemon SIGKILL during spawn, authentication, Active, and stop before
shipping.

On restart, restore only `auto_start` rules after obtaining the lock. Reconnect
does not imply restoration across daemon lifetimes. No `state.json` is needed
for version 1. Closing the TUI has no effect on the daemon or its guardians.

## Persistence and scope

Keep the existing versioned TOML and newline-delimited JSON contracts. Use UUID
v4 for new rule/request identities; names remain unique and editable. Use
`kind`, `bind_port`, `destination_host`, and `destination_port` consistently;
Remote uses the same fields with the listener on the remote host. Dynamic
omits destination fields. Port zero is outside version 1. Default binds are
`127.0.0.1`; non-loopback exposure must be an explicit user selection.

Use the OS-specific defaults and override rules in
[platform support](#platform-support). Reject unsafe paths rather than repairing
arbitrary existing directories. Config files use mode 0600. Tests inject all
paths and cover both OS defaults independently of the machine running the test.

Milestone 0 remains the first implementation scope. Before building the
Milestone 2 daemon around this process design, run an isolated real-OpenSSH
spike covering all three forwarding types, configured extra forwards, an
existing user master, Remote rejection, and guardian crash cleanup. A fake SSH
alone cannot validate OpenSSH control semantics. Keep this a separate explicit
integration check; ordinary unit tests remain offline.

### M1 configuration persistence implementation

`ConfigV1` now converts through the existing validated domain constructors and
rule-set validation. `ConfigStore` owns an open private directory descriptor;
file access, replacement, and cleanup are relative to it using portable Unix
`openat`/`renameat`/`unlinkat`. It is not a daemon lock. M2 must acquire the sole
writer lock before using mutation or abandoned-save cleanup APIs; CLI/TUI
execution paths remain unavailable in this change.

The path resolver takes explicit home/XDG inputs and a target platform instead
of reading process-global environment in tests. Relative config/state overrides
are ignored. A runtime override must be absolute, owned, non-symlink, and 0700;
otherwise the documented OS fallback applies. Application directories and files
are rejected when their existing permissions are unsafe, never repaired. File
access rejects symlinks, non-regular files, and multiple hard links. Ancestors
such as HOME, Library, and XDG roots are caller-selected infrastructure, not
application directories to chmod; newly created directories use 0700.

Saving validates before writing, creates an exclusive UUID temporary file at
0600, writes/flushes/fsyncs it, renames, then syncs the directory. Unknown or
invalid existing configurations are not overwritten. Failures before rename
retain the previous file and attempt to remove the temporary. A directory-sync
failure after rename reports `DurabilityUncertain`: the replacement is already
visible and rollback is not promised. Process death can leave private temporary
files; they are never loaded or reused. The sole writer may explicitly discard
an abandoned save by UUID once no live writer owns it, rather than scanning and
deleting another writer's files automatically.

Migration is an explicit interface, with no invented historical schema shipped.
Unknown versions are rejected by default, including all future schemas even
when a migration is supplied. A registered older-schema migration first writes
and syncs a unique 0600 backup of the exact original bytes and syncs its directory,
then converts and validates before atomic replacement. Migration failures retain
the original and the completed backup. Tests use a synthetic migration only.

The direct `libc` dependency and test-only `tempfile` dependency are dual
MIT/Apache-2.0 licensed according to their package manifests. No third-party
source or assets were copied; dependency packages retain their license files.
Native macOS verification remains required; Linux tests of macOS path selection
do not establish native filesystem behavior.

### M1 SSH host discovery implementation

Host discovery walks `~/.ssh/config` and user-config-relative `Include`
patterns without editing them. It lists only exact `Host` tokens, deduplicates
and sorts aliases, ignores negated and wildcard patterns, detects canonical-path
cycles, and bounds traversal to 32 include levels and 256 files. Missing or
unreadable files are non-fatal warnings so one stale include does not hide other
hosts; exceeding a traversal bound fails the refresh rather than returning a
silently incomplete catalog.

OpenSSH remains the settings and authentication authority. Details come from
bounded `ssh -F <selected-config> -G -- <alias>` output. Connection tests
directly execute a bounded `ssh -F <selected-config> -T -n` process with
`BatchMode=yes`, `ClearAllForwardings=yes`, a ten-second OpenSSH connect timeout,
and one connection attempt. Thus discovery, details, and connection tests use
the same selected config. They preserve the alias and do not override identity,
agent, proxy, or host-key policy. Timeout cleanup kills the dedicated process
group, including a configured proxy helper. Diagnostics are fixed, classified
messages; captured OpenSSH output is bounded and never returned or logged,
avoiding disclosure of user names, paths, endpoints, or remote banners. Unknown
and changed keys intentionally share one host-key verification category because
locale-independent stderr cannot safely distinguish every OpenSSH version.

The `glob` dependency is used only for OpenSSH-style Include path expansion.
Its package metadata declares MIT OR Apache-2.0, and no source or assets were
copied.

GH-3 local verification (2026-09-22): Linux x86_64, Rust 1.85.0;
`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
and `cargo test --all-features` pass (54 unit tests, 5 CLI tests, doc-tests).
Tests use an SSH config fixture and fake SSH executable; no personal config,
network account, or live server was used. Native macOS and remote CI remain
post-publication review conditions.

GH-19 local verification (2026-09-22): Linux x86_64, Rust 1.85.0;
`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
and `cargo test --all-features` pass (47 unit tests, 3 CLI tests, doc-tests).
Migration tests also verify that the backup exists before conversion begins and
invalid domain output preserves both the original configuration and its backup.
Filesystem tests force an actual rename failure and verify that abandoned-save
cleanup rejects symlinks without removing them or modifying the configuration.
The installed `/usr/local/cargo/bin` was added to PATH so rustdoc could run.
All configuration tests use temporary directories; platform-default tests only
construct paths. Linux/macOS GitHub CI has not run for these working-tree
changes, and native macOS remains unverified. Remote Linux/macOS CI is a
post-publication human-review condition; the local checks permit committing
this implementation but do not mark the issue complete.

### M2 daemon IPC foundation implementation

The public daemon socket uses the existing version 1 newline-JSON contract,
with a 1 MiB encoded-frame limit and five-second client/server I/O deadlines.
Malformed and oversized frames close only their connection after a structured
error. Version mismatch is rejected before dispatch. UUID request correlation
is checked by clients. Stop responses use a ten-second client deadline so the
five-second TERM grace and bounded guardian cleanup can finish without reporting
a false timeout; the daemon state lock is not held during that wait. A
subscription consumes its client connection and keeps
one buffered reader across the acknowledgement and subsequent event frames, so
already-buffered events are not discarded. Event sequence state is daemon-owned;
each subscriber has a 64-event bounded queue and is disconnected on lag.

On-demand clients spawn the hidden `daemon run` entry point only when connect
fails, then wait for readiness. Every contender first opens the permanent
0600 lock and obtains nonblocking `flock`; only its owner may remove a stale
current-user socket and bind a new 0600 socket. Unsafe runtime paths and socket
objects are rejected without permission repair. The lock file is never
unlinked. This preserves the later guardian inheritance boundary.

`DaemonManager` is the sole configuration writer. It loads validated rules at
startup, validates candidate rule sets before atomically persisting add/remove,
and keeps start-request intent in memory. Client-created duplicate names or
overlapping listeners return `Conflict`; persisted-data failures remain internal
errors. Repeated start/stop returns success with an explicit `changed` flag and
emits no duplicate event. The foundation CLI currently accepts UUIDs for rule
remove/start/stop; exact-name lookup remains part of the full CLI slice in GH-6.
This issue does not claim that a start launches OpenSSH; process supervision and
guardian cleanup remain the next Milestone 2 slice.

### M2 OpenSSH guardian implementation

Each start creates a mode-0700 attempt directory and launches the same binary
in hidden guardian mode. Only a private socketpair lease and a clone of the
daemon's locked open-file description are inherited. The validated rule is
sent over the socketpair without a shell; the guardian restores close-on-exec
before spawning SSH, so SSH children inherit neither descriptor.

The foreground master receives the accepted options and no forwarding flag.
Readiness requires a private `-O check`; Active is emitted only after a separate
configuration-free `-O forward` accepts the requested forwarding. IPv6 fields
are bracketed. Master stderr is continuously drained with the first 64 KiB
retained, control calls have five-second deadlines, and startup has the agreed
thirty-second deadline.

The guardian keeps the master group leader unreaped while signaling its group.
Lease EOF sends SIGTERM, waits five seconds, sends SIGKILL if necessary, reaps,
removes the attempt directory, and finally drops the inherited lock. Stop can
remove the Starting marker during connection; a later acceptance is discarded
and cleaned up without a delayed Active event. No old or persisted PID is used
as signaling authority.

GH-5 local verification (2026-09-23): Linux x86_64, Rust 1.85.0;
`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
and `cargo test --all-features` pass (71 unit tests, 5 CLI tests, 5 process
lifecycle tests, doc-tests). An isolated OpenSSH 10.5p1 probe using disposable
keys and configuration also passes strict host-key verification, private-master
isolation, Local/Remote/Dynamic forwarding with real traffic, conflicts,
cancellation, and daemon-SIGKILL guardian cleanup. Native macOS execution
remains tracked separately; the PR's macOS job validates this Rust path on a
native runner without claiming a full release-machine acceptance test.
