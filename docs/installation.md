# Installation and first Local forwarding

This guide is the detailed user installation and removal reference. The
README intentionally keeps only the shortest route here.

## Prerequisites and supported targets

TunnelDeck targets Linux `x86_64`/`aarch64` and macOS Intel/Apple Silicon,
with the baselines listed in the README. The initial installation route builds
from source and requires:

- Rust and Cargo 1.85 or newer compatible with the locked dependencies;
- Git when installing directly from the repository URL;
- the system OpenSSH client (`ssh`) at runtime. Apple's `/usr/bin/ssh` is the
  baseline on macOS.

Cargo installs executables to `$CARGO_HOME/bin` (normally `$HOME/.cargo/bin`).
Add that directory to `PATH` if `tdeck` is not found after installation.
Broader native macOS support is still under validation; see the
[native validation checklist](macos-validation.md).

## Install from Git on Linux or macOS

Use the locked repository dependency graph:

```sh
cargo install --git https://github.com/kshiva1126/tunnel-deck --locked
tdeck --version
tdeck --help
```

TunnelDeck is not currently published on crates.io. Do not use
`cargo install tunnel-deck`; that command would refer only to a crates.io
package and is not the documented installation route.

## Install from an existing clone

From the repository root, install the checked-out revision with the lock file:

```sh
cargo install --path . --locked
```

For an isolated installation, add `--root <directory>`; the executable is
written to `<directory>/bin/tdeck`. CI uses this form on both Linux and macOS
and runs the installed executable's version and help commands.

## Update

Stop active forwards before replacing the executable so an older daemon and
guardians are not left running beside a newer client. Then reinstall from Git:

```sh
cargo install --git https://github.com/kshiva1126/tunnel-deck --locked --force
```

For a clone, fetch and review the desired revision, then run
`cargo install --path . --locked --force` from its root. The next CLI or TUI
operation launches the newly installed per-user daemon.

## Prebuilt artifacts are a separate future path

The release workflow also exercises architecture-specific archives and their
checksums, but these prebuilt binaries are not the required initial
distribution route. A downloaded macOS binary is subject to Gatekeeper and
requires signing/notarization policy before that path is promoted. Those
requirements do not directly apply to a binary Cargo builds locally from
source. Never disable Gatekeeper globally.

## Prepare SSH authentication and host keys

TunnelDeck delegates connection behavior to the system `ssh` command and reads
aliases from `~/.ssh/config`. Before opening a forwarding rule:

1. Define a concrete `Host` alias in `~/.ssh/config` (included configuration is
   supported). TunnelDeck does not edit this file.
2. Prepare key-based, non-interactive authentication, normally with an
   unlocked key in `ssh-agent`. TunnelDeck does not store or prompt for SSH
   passwords or key passphrases.
3. Connect once in a terminal with `ssh <alias>`. Check the server fingerprint
   through a trusted channel before accepting an unknown host key. A changed
   key must be investigated; do not bypass verification.
4. Confirm TunnelDeck sees the alias with `tdeck host list`, then run
   `tdeck host test <alias>`.

TunnelDeck preserves OpenSSH's configured agent, identity, jump-host, and
known-hosts behavior. It never silently weakens host-key verification.

## Start and stop a Local forward

The shortest CLI workflow forwards local `127.0.0.1:3000` to port 3000 on the
selected SSH host:

```sh
tdeck forward add --name web --host server --bind-port 3000 --destination-port 3000
tdeck forward start web
tdeck status
tdeck forward stop web
```

Replace `server` with an alias from `tdeck host list`. Running `tdeck` opens
the TUI for the equivalent interactive workflow. Closing the TUI does not stop
an active daemon-managed forward; stop it explicitly in the TUI or CLI.

If the local port is occupied, the CLI returns a conflict and never kills the
listener or silently chooses another port. Choose another `--bind-port` when
adding the rule. The TUI may propose an available alternative, but applies it
only after confirmation. `Active` means OpenSSH accepted the forwarding; it
does not prove the destination service is healthy.

## Preview and import SSH forwarding directives

Inspect the effective `LocalForward`, `RemoteForward`, and `DynamicForward`
directives for an SSH alias without saving them:

```sh
tdeck forward import server
tdeck --json forward import server
```

Each candidate includes a numeric ID, forwarding type, bind, destination where
applicable, and a supported, duplicate, conflict, unsupported, or invalid
classification with its reason. Saving is never implicit. Pass only the IDs
you intend to save; repeat `--select` or use a comma-separated list:

```sh
tdeck forward import server --select 1,3
```

All selected candidates are validated and saved as one operation. If any
selection is unavailable or persistence fails before atomic replacement, none
are saved. An uncertain-durability error means replacement completed but its
directory sync failed; use `tdeck forward list` to inspect the reconciled batch.
Import does not start a connection and never rewrites `~/.ssh/config`; start an
imported rule later with `tdeck forward start <name-or-uuid>`. Shell completion
and the generated man page include this subcommand automatically because both
are rendered from the same command definition as `--help`.

## Configuration, logs, and runtime files

| Platform | Configuration | Log |
| --- | --- | --- |
| Linux | `${XDG_CONFIG_HOME:-$HOME/.config}/tunnel-deck/config.toml` | `${XDG_STATE_HOME:-$HOME/.local/state}/tunnel-deck/tunnel-deck.log` |
| macOS | `$HOME/Library/Application Support/TunnelDeck/config.toml` | `$HOME/Library/Logs/TunnelDeck/tunnel-deck.log` |

Absolute `XDG_CONFIG_HOME` and `XDG_STATE_HOME` override the defaults on both
platforms. Runtime socket/lock data uses a private `tunnel-deck` directory
under a safe absolute `XDG_RUNTIME_DIR`; otherwise it falls back to
`/tmp/tunnel-deck-<uid>` on Linux or `/private/tmp/tunnel-deck-<uid>` on macOS.
TunnelDeck rejects unsafe permissions or symlinked application paths rather
than silently repairing them. Logs contain classified lifecycle diagnostics,
not raw SSH stderr or credentials.

## Shell completion and manual page

Generate completion directly from the same command definition used by
`--help`:

```sh
# Bash (current user)
mkdir -p "$HOME/.local/share/bash-completion/completions"
tdeck completion bash > "$HOME/.local/share/bash-completion/completions/tdeck"

# Zsh (choose a directory already present in $fpath)
tdeck completion zsh > /path/in/fpath/_tdeck

# Fish
mkdir -p "$HOME/.config/fish/completions"
tdeck completion fish > "$HOME/.config/fish/completions/tdeck.fish"
```

PowerShell and Elvish generators are also available; list accepted values with
`tdeck completion --help`. Generate and install the manual page with:

```sh
mkdir -p "$HOME/.local/share/man/man1"
tdeck manpage > "$HOME/.local/share/man/man1/tdeck.1"
man "$HOME/.local/share/man/man1/tdeck.1"
```

Release packaging can run these commands to ship generated files without
maintaining a second command specification.

## Uninstall

Stop every rule first (`tdeck forward list`, then `tdeck forward stop <rule>`),
and ensure no `tdeck` TUI is open. Remove the Cargo-installed executable, then
remove completion and manual files from any locations you chose. Remove
configuration and logs only if you do not want to retain rules or diagnostics:

```sh
cargo uninstall tunnel-deck
rm -f "$HOME/.local/share/bash-completion/completions/tdeck"
rm -f "$HOME/.local/share/man/man1/tdeck.1"
```

If installation used `--root <directory>`, pass the same option to uninstall:
`cargo uninstall --root <directory> tunnel-deck`.

For data removal, use the paths in the preceding table (including any XDG
overrides you selected). Runtime files normally disappear with the daemon; if
they remain after every TunnelDeck process has exited, the platform-specific
private runtime directory may be removed. Do not remove runtime files while a
forward or daemon is active.
