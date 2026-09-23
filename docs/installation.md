# Installation and first Local forwarding

This guide is the detailed user installation and removal reference. The
README intentionally keeps only the shortest route here.

## Supported targets and current limitations

TunnelDeck targets Linux `x86_64`/`aarch64` and macOS Intel/Apple Silicon,
with the baselines listed in the README. Release automation natively checks that
each packaged executable renders version/help, completion, and manpage output.
Broader macOS support is still under validation; see the
[native validation checklist](macos-validation.md). In particular, macOS 13 and
Intel hardware acceptance, signing/notarization, Gatekeeper, real SSH, and TUI
checks remain pending.

Release archives are named `tunnel-deck-<version>-<target>.tar.gz`. Each
contains a same-named directory with `tdeck`, `LICENSE`,
`THIRD_PARTY_LICENSES.txt`, and `release.json`.
The Release also provides `SHA256SUMS` and `release-manifest.json`. Select the
target for your operating system and CPU; do not substitute an archive for
another architecture.

## Install or update on Linux

1. Open the desired entry on the repository's
   [Releases page](https://github.com/kshiva1126/tunnel-deck/releases).
2. Download `SHA256SUMS` and the archive for
   `x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`. Substitute the
   Release version and target below, then verify and extract it:

   ```sh
   version=0.1.0
   target=x86_64-unknown-linux-gnu
   archive="tunnel-deck-${version}-${target}.tar.gz"
   grep "  ${archive}$" SHA256SUMS | sha256sum --check -
   tar -xzf "$archive"
   ```

3. Install the extracted executable in a directory on `PATH`:

   ```sh
   install -Dm755 "tunnel-deck-${version}-${target}/tdeck" "$HOME/.local/bin/tdeck"
   "$HOME/.local/bin/tdeck" --version
   ```

   Add `$HOME/.local/bin` to `PATH` if your shell does not already include it.

To update, stop any active forwards first, download and verify the desired
Release, then replace the executable with the same `install` command. Starting
the next CLI/TUI operation launches the updated per-user daemon. This explicit
stop avoids leaving an older daemon and guardians alive while replacing the
client binary.

## Install or update on macOS

1. Open the desired entry on the
   [Releases page](https://github.com/kshiva1126/tunnel-deck/releases).
2. Download `SHA256SUMS` and the archive for Apple Silicon
   (`aarch64-apple-darwin`) or Intel (`x86_64-apple-darwin`). Substitute the
   Release version and target below, then verify and extract it:

   ```sh
   version=0.1.0
   target=aarch64-apple-darwin
   archive="tunnel-deck-${version}-${target}.tar.gz"
   grep "  ${archive}$" SHA256SUMS | shasum -a 256 --check -
   tar -xzf "$archive"
   ```

3. Install the extracted executable:

   ```sh
   mkdir -p "$HOME/.local/bin"
   install -m755 "tunnel-deck-${version}-${target}/tdeck" "$HOME/.local/bin/tdeck"
   "$HOME/.local/bin/tdeck" --version
   ```

TunnelDeck is currently unsigned and not notarized. Gatekeeper can therefore
block the downloaded executable. Inspect the Release source and checksum, then
use Finder's per-application **Open** confirmation or the corresponding
per-file approval in System Settings if you trust it. Do not disable
Gatekeeper globally. Distribution signing/notarization and broader native
acceptance checks remain release work.

Update using the same stop, verify, and replacement sequence as Linux.

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
and ensure no `tdeck` TUI is open. Remove the installed binary, completion, and
manual files from the locations chosen above. Remove configuration and logs
only if you do not want to retain rules or diagnostics:

```sh
rm "$HOME/.local/bin/tdeck"
rm -f "$HOME/.local/share/bash-completion/completions/tdeck"
rm -f "$HOME/.local/share/man/man1/tdeck.1"
```

For data removal, use the paths in the preceding table (including any XDG
overrides you selected). Runtime files normally disappear with the daemon; if
they remain after every TunnelDeck process has exited, the platform-specific
private runtime directory may be removed. Do not remove runtime files while a
forward or daemon is active.
