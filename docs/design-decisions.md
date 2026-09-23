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

The Python probe shares its OpenSSH protocol and guardian harness across Linux
and macOS. Guardian completion uses explicit pipe IPC and observable listener
and lock state; it does not use `/proc`, `prctl`, or subreapers. The macOS CI
job runs it with Apple's `/usr/bin/ssh`. Native job evidence, release-artifact
execution on both architectures, and human TUI/browser/install checks are still
required before claiming release-verified macOS support; see
[the macOS validation checklist](macos-validation.md).

### M5 release artifact generation

The release workflow builds the four platform triples from one source revision
with Rust 1.85.0 and `Cargo.lock`. Linux GNU artifacts are built on native
Ubuntu 22.04 x86_64 and arm64 runners to retain the selected glibc 2.35
baseline. macOS builds use native Intel and Apple Silicon runners and set
`MACOSX_DEPLOYMENT_TARGET=13.0`. Linux artifacts dynamically link glibc and
macOS artifacts dynamically link
libSystem; the packager verifies those properties with native object-file tools.

Archive names include the package version and full target triple. A source
commit timestamp supplies every tar and gzip timestamp, ownership is normalized,
and platform build identifiers are disabled. Each target emits a JSON evidence
record and SHA-256 sidecar. The aggregate step refuses missing, duplicate, or
checksum-mismatched targets, then emits `SHA256SUMS` and a single release
manifest in a fixed target order.

Pull requests and manual dispatches run the complete build and aggregation
without release-write permission. Only a `v*` tag enables final GitHub Release
publication. Action references use immutable commit IDs. Release evidence keeps
`build.status` and `native_smoke_test.status` separate. Each native build job
extracts a candidate archive and runs its `tdeck` with `--version`, `--help`,
Bash completion generation, and manpage generation. Only after those checks
pass does it embed a machine-readable `passed` record containing the target,
runner OS/architecture, source revision, and individual checks. Because that
record changes the archive, the job extracts and runs the final archive again
before allowing upload. Archive `release.json` and the per-target JSON sidecar
are byte-equivalent; a separate checksum sidecar avoids a self-referential
archive digest. Aggregation rejects any target without passed evidence, so pull
requests and tags use the same gate and smoke failure prevents publication.

This smoke proves only that the packaged CLI starts and renders static
command-derived output on the four current native runners. Signing,
notarization, Gatekeeper, the macOS 13 minimum, real SSH traffic, and interactive
TUI behavior remain separate release acceptance work.

### M5 release audit and notices

GH-50 audits the four-target locked dependency union rather than only the host
graph. A deterministic generator retains each reachable package's upstream
license/notice files in `THIRD_PARTY_LICENSES.txt`; release archives contain
that file beside TunnelDeck's MIT `LICENSE`. A behavior test regenerates and
compares the notice bytes and verifies both files are packaged. This was chosen
over a hand-maintained allow-list because dependency changes could otherwise
leave required attribution stale. The audit classification, RustSec reachability
analysis, resource-limit gaps, commands, versions, and native/human unknowns are
recorded once in [the release audit](release-audit.md).

No IPC, persistence, authentication, or process-ownership contract changes in
this audit. Same-user connection/thread and configured guardian counts remain
OS-limited rather than application-limited; future caps require a focused
compatibility-reviewed hardening change. macOS 13 and Intel validation, signing,
and notarization remain release-owner work.

### M5 initial distribution through Cargo install

The required initial distribution path is a source build with Cargo, not a
downloaded prebuilt executable. Linux and macOS users install the published
crate with `cargo install tunnel-deck --locked`, build the selected Git branch
with locked dependencies using `cargo install --git ... --locked`, or install a
reviewed clone with `cargo install --path . --locked`. A fixed Git source
revision requires an explicit `--rev <commit>`. This keeps the Rust 1.85 and
system OpenSSH prerequisites explicit. GH-67 adds crates.io as a distribution
source while retaining the Git route for branch or revision-specific installs.

Publishing version 0.1.0 on crates.io does not require pushing a `v0.1.0` Git
tag. The existing tag workflow would also create a prebuilt GitHub Release,
which has separate signing and acceptance work under GH-51. Record the exact
published commit on GH-67; create a release tag when that prebuilt path is ready.

The ordinary Linux/macOS CI matrix installs the checkout into an isolated
Cargo root, then runs the installed `tdeck` version and help entry points. It
also checks `cargo package --list` for the manifest, README, MIT license, and
binary/library source entry points. This verifies package/install mechanics;
it does not replace native SSH, TUI, or minimum-OS acceptance.

Gatekeeper, signing, and notarization govern downloaded prebuilt macOS
binaries and are retained as future prebuilt-distribution requirements. They
do not directly govern a binary compiled locally from source by Cargo. The
existing archive workflow remains useful prebuilt-path evidence, but is not a
prerequisite for satisfying the initial Cargo install distribution path. No
application, persistence, IPC, authentication, or process-ownership boundary
changes as a result of this distribution decision.

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

Application settings are daemon-owned desired configuration in the same
`config.toml` atomic-write transaction as rules. Schema version 2 adds a
`[settings]` table containing `theme` (`system`, `dark`, or `light`),
`log_level` (`error` through `trace`), `default_reconnect`, and
`default_auto_start`. Defaults are `system`, `info`, and `false` for both
policies. Policy defaults initialize newly created rules; changing a default
does not rewrite existing rule policy.

The daemon remains the sole writer. IPC v1 adds `settings_get` and
`settings_update`; CLI and TUI use those operations and never write TOML
directly. The protocol version remains 1 because existing message and error
shapes do not change; an older daemon rejects the unknown operation, prompting
the client to report that the daemon must be upgraded or restarted.

Daemon startup explicitly registers the only production migration: TOML v1
rules become TOML v2 with default settings. Before conversion, the exact v1
bytes are stored in a uniquely named, synced mode-0600 backup. Conversion or
validation failure leaves the original in place. Versions other than 1 or 2
are rejected and never overwritten or guessed.

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

Migration is an explicit interface. Schema v1 is the only registered historical
schema; unknown versions are rejected by default, including all future schemas
even when another migration is supplied. A registered older-schema migration
first writes
and syncs a unique 0600 backup of the exact original bytes and syncs its directory,
then converts and validates before atomic replacement. Migration failures retain
the original and the completed backup. Tests cover both the registered v1
migration and a synthetic failing migration.

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

### M6 effective-forward import preview

The first SSH-config import slice reuses the bounded `ssh -F <config> -G --
<alias>` query and consumes only its effective `localforward`, `remoteforward`,
and `dynamicforward` lines. The application layer returns an ordered, typed
preview and has no persistence, IPC, process, or logging effect. Callers supply
immutable snapshots of TunnelDeck rules and running rule IDs; the preview does
not allocate rule IDs or names because selection and saving belong to a later
issue.

OpenSSH's bracketed IPv4/IPv6 output is normalized to unbracketed domain values.
An omitted Local/Dynamic bind address is represented as `*` when the effective
client `GatewayPorts` value is `yes`, and as `localhost` otherwise. An omitted
Remote bind remains `localhost` because the remote server's policy is separate.
Exact repeated effective directives and exact existing rules are duplicates. A different Local/Dynamic
candidate whose local listener overlaps an earlier supported candidate or an
existing rule is a conflict. Remote listeners conflict only for the same SSH
alias; they do not consume a local listener. An exact existing rule reports
whether its supplied ID is currently running, without inspecting or changing
daemon state.

Unix-domain forwarding and destination-less RemoteForward (remote SOCKS) are
classified as unsupported because TunnelDeck's versioned rule model cannot
represent them; guessing a mapping or changing storage/IPC would violate this
slice. Missing fields, zero/out-of-range ports, invalid bind addresses, invalid
destination hosts, and non-UTF-8 query output are invalid with fixed typed
reasons. The preview retains no raw diagnostic and does not log expanded
effective values, which may contain sensitive paths or endpoints.

### M6 CLI import selection and atomic persistence

`tdeck forward import <alias>` is non-interactive and previews by default.
Candidate IDs are the one-based positions in the ordered effective-forward
preview. Saving requires one or more explicit `--select <id>` values; repeated
options and comma-separated IDs are accepted. Unsupported, invalid, duplicate,
or conflicting candidates cannot be selected. Generated rule names use the
forwarding type and bind port, with deterministic numeric suffixes when needed;
new UUIDs are allocated only after selection. Policy values use the persisted
new-rule defaults.

The CLI obtains desired and running snapshots from the daemon, runs the bounded
effective query, and sends all selected rules in one IPC v1 `forward_import`
request. Adding an operation follows the existing v1 extension rule: an older
daemon rejects the unknown operation and must be restarted or upgraded. The
daemon converts every wire rule through the existing domain constructors,
validates the complete resulting rule set, and calls the existing atomic config
save once. It replaces in-memory desired state and publishes one configuration
event only after the file is atomically replaced. A failure before replacement
leaves memory and disk unchanged. A directory-sync failure after replacement is
reported as uncertain durability, but the daemon reconciles memory and emits the
event because the complete batch is already visible on disk. Repeating
`forward_add` was rejected because a later failure could leave an earlier
candidate saved. Import does not call start, alter running attempts, or edit SSH
configuration.

### M6 TUI import selection

The host list and the selected rule's host can open the same effective-forward
preview used by the CLI. The TUI keeps cursor and selection state locally,
marks unsupported, invalid, duplicate, and conflicting candidates as
unselectable, and requires a separate confirmation after at least one supported
candidate is selected. It uses the shared application-layer selection and
deterministic naming logic, then sends exactly one daemon `forward_import`
mutation. It never sends `forward_start` as part of import.

Escape/cancel, preview-query failure, terminal failure, and a failed daemon
mutation do not change desired rules, running attempts, or SSH configuration.
On mutation failure the preview remains open with the daemon diagnostic so the
user can review or cancel. This preserves the existing daemon as sole writer
and the terminal RAII cleanup boundary; no storage, IPC, authentication, or
process-ownership contract changes are introduced.

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

### M3 terminal UI implementation

The TUI remains an IPC client: the daemon owns desired rules and SSH attempts,
and closing the terminal drops only client connections. The common form creates
Local forwarding with both bind and destination addresses set to `127.0.0.1`;
the remote port initially drives the local port. A local bind probe may offer
the next available port, but only an explicit key accepts it. The daemon probes
again at start, so a race is reported instead of silently changing the saved
port or stopping another listener.

An existing stopped rule is updated by sending its stable ID through the
existing `forward_add` operation. The daemon treats that ID as an atomic
replacement after validating the whole candidate rule set; an active rule
rejects editing. This preserves the version 1 operation and payload shape while
keeping the daemon the sole writer. Duplication uses a fresh UUID, and deletion
retains a confirmation step.

Terminal ownership is scoped by an RAII guard. Normal/error returns restore raw
mode, alternate screen, and cursor; the panic hook restores them before the
original panic reporter runs, and SIGINT/SIGTERM/SIGHUP request an orderly loop
exit. Browser launching is isolated in `platform`, uses `xdg-open` on Linux and
`open` on macOS, passes the displayed HTTP/HTTPS URL as one argument without a
shell, and never guesses a protocol automatically.

Ratatui 0.29, Crossterm 0.28, and signal-hook 0.3 were added from crates.io;
their package manifests declare MIT-family compatible licensing and no
third-party source or assets were copied. Native macOS terminal, signal, and
browser behavior still requires post-publication validation.

### M4 runtime recovery and diagnostics

The daemon, rather than a client or guardian, owns automatic recovery. Only
persisted rules with `auto_start` are restored when a daemon starts. A rule with
`reconnect` uses capped exponential full jitter with a 60-second ceiling; a
successful 60-second run resets the retry attempt. Manual stop records intent
before cancelling a pending or active attempt, so a concurrent retry cannot
restart it. Guardians continue to own exactly one OpenSSH attempt and never
implement retry policy.

Only recognized OpenSSH failure patterns become fixed authentication,
host-key, listener, network, or remote-rejection diagnostics. Raw stderr is not
returned or logged, and an unrecognized failure remains `unknown`. Unexpected
exit of a previously active attempt is likewise not asserted to be a network
failure. Runtime status exposes uptime, reconnect count, and the last redacted
diagnostic for the TUI and CLI.

The daemon writes rule start, classified failure, reconnect, and stop events to
the bounded rotating log. It reads the daemon-owned current setting for every
record, so a persisted `log_level` update takes effect without restarting;
failures use `error` and lifecycle events use `info`. Log records contain only
rule IDs and application-owned fixed diagnostic classifications/messages, never
captured OpenSSH stderr, rule connection fields, credentials, or environment
values. CLI and TUI diagnostics continue to read the same `RuntimeInfo` message
used to form failure/reconnect records.

The log directory and every active/rotation file must remain private regular
objects owned by the current user; symlinks, hardlinks, wrong modes, and unsafe
rotation slots reject startup or the affected record. The logger retains the
validated private-directory descriptor and performs append, removal, and
rotation with descriptor-relative operations, so replacing a parent path cannot
redirect log output. A process-local writer lock serializes size checking,
rotation, and append. Rotation keeps three 1 MiB generations, and logging
failure remains observational: it cannot roll back an already completed tunnel
transition. GH-42's TOML v2 and IPC v1 settings shapes are unchanged.
