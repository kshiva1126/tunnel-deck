# TunnelDeck

TunnelDeck is a Linux-first terminal user interface for configuring,
controlling, and monitoring SSH port forwarding.

- Product name: **TunnelDeck**
- Repository and Rust package: `tunnel-deck`
- Executable: `tdeck`
- Planned implementation: Rust with Ratatui and Tokio

The goal is to make common SSH tunnel operations accessible from a coherent
TUI while retaining a concise, scriptable CLI.

> This repository currently contains planning documentation only. No
> application scaffold or source code has been created yet.

## Intended experience

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

- [Product specification](docs/product-spec.md)
- [Architecture](docs/architecture.md)
- [Implementation plan](docs/implementation-plan.md)
