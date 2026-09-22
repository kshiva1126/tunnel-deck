# TunnelDeck

TunnelDeck is a Linux and macOS terminal user interface for configuring,
controlling, and monitoring SSH port forwarding.

- Product name: **TunnelDeck**
- Repository and Rust package: `tunnel-deck`
- Executable: `tdeck`
- Planned implementation: Rust with Ratatui and Tokio

The goal is to make common SSH tunnel operations accessible from a coherent
TUI while retaining a concise, scriptable CLI.

> The Milestone 0 Rust scaffold and compatibility fixtures are implemented.
> SSH discovery, forwarding, daemon, and TUI operations remain explicitly
> unavailable while their milestone issues are open.

## Intended experience

The primary workflow is to run TunnelDeck on your local computer, choose a
host from SSH config, and forward a remote development-server port to a local
address you can open in your browser. If the local port is occupied, the TUI
offers another port for you to select. Remote port auto-discovery is not required.

Initial release targets are Linux (`x86_64`, `aarch64`) and macOS (Apple Silicon
and Intel). These are planned targets, not claims of completed platform testing.
The current baseline is Rust 1.85, Linux kernel 5.15 with glibc 2.35, and macOS
13. Native runtime validation of the release targets remains tracked in the
roadmap; CI compilation alone is not treated as that validation.

Running `tdeck` opens the dashboard. A user should be able to discover SSH
hosts, create a forwarding rule with a form, validate it, start or stop it,
inspect failures, and change application settings without editing files by
hand.

The CLI remains available for automation:

```text
tdeck                         Open the TUI
tdeck host list               List discovered SSH hosts
tdeck forward list            List forwarding rules
tdeck forward start <name>    Start a rule
tdeck forward stop <name>     Stop a rule
tdeck status                  Show a concise status summary
```

The exact command tree should be validated during the first implementation
milestone rather than treated as frozen.

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

## License

TunnelDeck is licensed under the [MIT license](LICENSE).
See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution terms.
