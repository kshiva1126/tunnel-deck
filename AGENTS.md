# AGENTS.md

## Communication

- Think in English and always respond to the user in Japanese.
- Lead with the result and keep progress updates concise.

## Project scope

- This repository is **TunnelDeck**, an independent Rust application.
- The product name is `TunnelDeck`, the repository/package name is
  `tunnel-deck`, and the executable command is `tdeck`.
- Build a Linux-first TUI for managing SSH port forwarding. Keep a useful CLI
  for scripting and automation.
- Verify the license and preserve all required notices before reusing any
  third-party code or assets.

## Before implementation

- Read `README.md` and every document under `docs/`.
- Resolve open design decisions in `docs/implementation-plan.md` before
  committing to an incompatible storage or IPC format.
- Prefer small, testable modules and keep the UI independent from SSH process
  management.
- Do not silently weaken SSH host-key verification.

## Expected quality

- Run `cargo fmt --check`, `cargo clippy --all-targets --all-features`, and
  `cargo test` before reporting implementation complete.
- Avoid panics in normal runtime paths and restore the terminal on errors.
- Never log passwords, passphrases, private keys, or sensitive environment
  values.
