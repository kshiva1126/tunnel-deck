# TunnelDeck

TunnelDeck is a Linux and macOS terminal user interface for configuring,
controlling, and monitoring SSH port forwarding.

- Product name: **TunnelDeck**
- Repository and Rust package: `tunnel-deck`
- Executable: `tdeck`
- Implementation: Rust with Ratatui and Crossterm

The goal is to make common SSH tunnel operations accessible from a coherent
TUI while retaining a concise, scriptable CLI.

> SSH host discovery, per-user daemon IPC, Local OpenSSH forwarding supervision,
> and the host-to-port TUI workflow are implemented.

## Intended experience

The primary workflow is to run TunnelDeck on your local computer, choose a
host from SSH config, and forward a remote development-server port to a local
address you can open in your browser. If the local port is occupied, the TUI
offers another port for you to select. Remote port auto-discovery is not required.

Initial release targets are Linux (`x86_64`, `aarch64`) and macOS (Apple Silicon
and Intel). These are planned targets, not claims of completed platform testing.
The current baseline is Rust 1.85, Linux kernel 5.15 with glibc 2.35, and macOS
13. Native runtime validation of the release targets remains tracked in the
roadmap; CI compilation alone is not treated as that validation. macOS support
is currently **under validation**; automated and human evidence is tracked in
the [macOS validation checklist](docs/macos-validation.md).

Running `tdeck` opens the dashboard. A user should be able to discover SSH
hosts, create a forwarding rule with a form, validate it, start or stop it,
inspect failures, and change application settings without editing files by
hand.

The CLI remains available for automation:

```text
tdeck                         Open the TUI
tdeck host list               List discovered SSH hosts
tdeck forward add --name web --host server --bind-port 3000 --destination-port 3000
tdeck forward add --kind remote --name callback --host server --bind-port 9000 --destination-port 9000
tdeck forward add --kind dynamic --name socks --host server --bind-port 1080
tdeck forward import server                 # preview effective SSH forwards
tdeck forward import server --select 1,3    # save only candidates 1 and 3
tdeck forward list            List forwarding rules
tdeck forward start <name-or-uuid>    Start a rule
tdeck forward stop <name-or-uuid>     Stop a rule
tdeck status                  Show a concise status summary
```

Add `--json` to any command for machine-readable success output. Daemon errors
are also reported as one JSON value on stderr; other errors remain plain text.
Daemon errors use stable exit statuses: 2 for invalid input, 4 for a missing
rule, 5 for a conflict, 6 for an unavailable operation, and 70 for an internal
failure.

## Install and get started

The initial distribution path is a locked source build with Cargo on Linux and
macOS. Install Rust 1.85 and OpenSSH, then run:

```sh
cargo install tunnel-deck --locked
tdeck --version
```

The executable is named `tdeck`. See the
[installation guide](docs/installation.md) for installing from Git or a clone,
updating, uninstalling, prerequisites, and SSH preparation.

## Prebuilt release artifacts

The `Release artifacts` GitHub Actions workflow builds archives for
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-apple-darwin`, and `aarch64-apple-darwin`. It runs for pull requests
and manual dispatches as well as `v*` tags, so the same release path is checked
before publication. Download the `tunnel-deck-release` workflow artifact to
obtain the four deterministic archives, `SHA256SUMS`, and
`release-manifest.json`.

The manifest records the target triple, dynamic-linking model, minimum OS,
build result, and packaged-artifact native-smoke status separately. Every target
archive is extracted on its matching native runner and its final `tdeck` runs
version/help plus completion and manpage generation before aggregation. Tag runs
create a GitHub Release only after all four smoke records, outputs, and checksums
have been validated. Every archive also includes TunnelDeck's `LICENSE` and the
locked dependency notices in `THIRD_PARTY_LICENSES.txt`; the pre-publication
evidence and remaining risks are recorded in the
[release audit](docs/release-audit.md). Signing, notarization, Gatekeeper, real
SSH/TUI behavior, and minimum-version hardware acceptance are separate checks
for a future prebuilt distribution path. They do not apply directly to a
`cargo install` build produced locally from source. Prebuilt archives are not
the required initial installation route, and TunnelDeck never requires
disabling Gatekeeper globally.

## Documents

Track work and implementation order in the
[GitHub Issues roadmap](https://github.com/kshiva1126/tunnel-deck/issues/13).
Issues own progress and acceptance checks; the documents below retain detailed
specifications and design decisions.
See [the contribution workflow](CONTRIBUTING.md#issue-to-pr-workflow) for
AI-assisted exploration, reviewable changes, verification, and code walkthroughs.
The optional [Symphony workflow](docs/symphony.md) can turn a labeled GitHub
Issue into an isolated Codex run, remediate its pull request, and safely merge
ordinary successful work; documented exceptional cases stop for human review.

- [Product specification](docs/product-spec.md)
- [Architecture](docs/architecture.md)
- [Implementation plan](docs/implementation-plan.md)
- [Accepted design decisions](docs/design-decisions.md)
- [OpenSSH experiment results and reproduction](docs/openssh-probe.md)
- [macOS automated and human validation](docs/macos-validation.md)
- [Installation, update, CLI extras, and removal](docs/installation.md)

## License

TunnelDeck is licensed under the [MIT license](LICENSE).
See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution terms.
