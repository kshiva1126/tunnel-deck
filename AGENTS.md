# AGENTS.md

## Communication

- Think in English and always respond to the user in Japanese.
- Lead with the result and keep progress updates concise.

## Project scope

- This repository is **TunnelDeck**, an independent Rust application.
- The product name is `TunnelDeck`, the repository/package name is
  `tunnel-deck`, and the executable command is `tdeck`.
- Build a Linux and macOS TUI for managing SSH port forwarding. Keep a useful CLI
  for scripting and automation.
- Treat both platforms as initial release targets. Isolate OS-specific paths
  and process operations; do not require Linux-only APIs for core behavior.
- Verify the license and preserve all required notices before reusing any
  third-party code or assets.

## Before implementation

- Read `README.md` and every document under `docs/`.
- Resolve open design decisions in `docs/implementation-plan.md` before
  committing to an incompatible storage or IPC format.
- Prefer small, testable modules and keep the UI independent from SSH process
  management.
- Do not silently weaken SSH host-key verification.

## Issue-driven AI workflow

- Follow the workflow in `CONTRIBUTING.md`. Track work through GitHub Issues;
  split broad issues into reviewable changes rather than one large PR.
- Before editing, inspect the relevant code/tests and briefly explain the
  affected modules, intended behavior, and verification approach. For a new
  module, say that no existing execution path exists yet.
- Continue routine implementation within the authorized scope without a new
  approval gate. Consult the owner before changing agreed product behavior,
  storage/IPC compatibility, authentication policy, or architecture boundaries.
- Keep each PR focused on one explainable behavior. Preserve unrelated work
  and avoid bundling opportunistic refactors.
- Explain nontrivial changes using actual code links: entry point, state owner,
  external effects, and failure cleanup. Do not describe planned code as built.
- Tie affected invariants to behavior tests. Never weaken assertions, skip a
  failing test, or rewrite expectations merely to make the implementation pass.
  Explain any intentional test removal or expectation change against the spec.
- Distinguish design decisions, implemented behavior, executed verification,
  and remaining unknowns. Linux success does not prove macOS behavior.
- Keep decision reasons and tradeoffs in the existing design documents, and
  update them with behavioral changes. Avoid duplicating specs across files.
- Do not close an issue just because code was generated or local tests passed;
  check its full acceptance criteria and record outstanding work explicitly.

## Expected quality

- Run `cargo fmt --check`, `cargo clippy --all-targets --all-features`, and
  `cargo test` before reporting implementation complete.
- Avoid panics in normal runtime paths and restore the terminal on errors.
- Never log passwords, passphrases, private keys, or sensitive environment
  values.
