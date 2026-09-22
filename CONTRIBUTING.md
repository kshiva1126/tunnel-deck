# Contributing

Contributions are accepted under the project's [MIT license](LICENSE).
By submitting a contribution, you agree to license it under those terms.
No contributor license agreement is required. Submit only work you have the
right to contribute, and preserve required third-party notices.

Read `AGENTS.md`, `README.md`, and all documents in `docs/` before implementation.
Keep changes scoped to a milestone and document changes to configuration or IPC
contracts. Never include credentials or personal SSH configuration in fixtures.

Once the Rust package exists, run these checks before submitting code:

```text
cargo fmt --check
cargo clippy --all-targets --all-features
cargo test
```

Use isolated temporary directories and fake SSH processes for default tests.
Document real-OpenSSH integration checks separately.

## Issue to PR workflow

Use [the roadmap](https://github.com/kshiva1126/tunnel-deck/issues/13) to find
work. Issues own scope, dependencies, and progress; `docs/` owns the product
contract and design. Neither a chat transcript nor an AI summary replaces the
current code and recorded test results.

Submit changes through pull requests rather than direct pushes to `main`. CI
runs automatically for pull requests and may be started manually when a
post-merge check is needed. This avoids repeating the same Linux/macOS jobs on
the resulting merge commit.

1. **Choose a bounded change.** Read the issue, acceptance criteria, and
   dependencies. Large issues are work packages, not mandatory single PRs.
   Split them into linked child issues or explicit PR-sized steps with one
   observable behavior each. Avoid arbitrary line-count limits.
2. **Explore and explain.** Inspect actual entry points, state owners, similar
   code, and tests. Before editing, share a short plan covering affected
   modules, purpose, impact, and verification. Routine changes can proceed
   immediately; changes to agreed behavior, compatibility, authentication, or
   architecture boundaries require discussion before dependent implementation.
3. **Implement and verify.** Keep the change focused. Derive tests from the
   acceptance criteria and failure cases, not just the shape of the code.
   Run the required Rust checks for implementation changes plus relevant
   integration checks. Documentation-only changes need link/diff consistency
   checks, not fabricated Rust test results.
4. **Explain the result.** Use the PR template and write descriptions in
   Japanese. Explain the before/after behavior, important execution path,
   state ownership, failures, and 2–3 code locations worth reading for a
   substantial change. Simple changes need only a short explanation; remove
   irrelevant template sections. Link to committed code, not local file paths.
5. **Review and complete.** Check implementation against intent and evidence.
   Give deeper attention to process signaling, concurrent start/stop, IPC
   permissions, persistence/migrations, and authentication. Resolve material
   findings before merging. Close an issue only when the integrated result
   meets all its acceptance criteria; a partial PR should use `Refs #N`, not
   an automatic closing keyword. Keep follow-up gaps visible.

An AI review is additional feedback, not proof of correctness. Reviewers should
be able to explain the important behavior and its failure handling; a mandatory
quiz or approval for every small edit is unnecessary. If the explanation is
unclear, trace one concrete scenario through the code before accepting it.

## Verification evidence

Record commands, outcomes, tested OS/tool versions where relevant, and checks
not run with their reasons. Separate automated CI checks, real-SSH checks, and
human TUI/installation checks. A passing build or fake process test does not
prove OpenSSH control semantics. macOS remains unverified until its required
native checks run; never mark it passed based on Linux or cross-compilation.

Maintain these behavioral obligations as the implementation grows:

| Invariant | Required evidence when affected |
| --- | --- |
| Closing the TUI leaves forwarding active | Client disconnect followed by traffic and control from a new client |
| Manual stop cancels reconnect | Timer/stop race test showing no later child launch |
| One attempt owns at most one managed master | Concurrent start/retry tests with child counts and attempt IDs |
| Occupied ports are not taken over or silently replaced | Competing listener stays alive; conflict returned; TUI selection changes only local port |
| Host-key policy is not weakened | Argument checks plus isolated unknown/changed-key rejection |
| Daemon crash cleans up within the documented guarantee | Crash scenarios with listener, child, and inherited-lock checks on each OS |
| Only the daemon writes valid desired configuration | Concurrent mutation and interrupted-write tests in temporary directories |
| Secrets do not enter diagnostics | Synthetic secret fixtures and log/redaction assertions |

This table specifies obligations, not tests that already exist. Each affected
PR links the actual tests and their results; do not maintain a duplicate global
test inventory or add tests for unrelated invariants on every change.

## Preserve reasons and understanding

Record significant decisions in `docs/design-decisions.md`: the problem,
selected approach, relevant alternatives/tradeoffs, and evidence or unknowns.
Update `docs/architecture.md` when boundaries or ownership change. Label
planned sections clearly until implemented; explain actual execution paths
with code links in PRs rather than claiming the planned diagram is running code.

Use focused commits whose messages explain what changed and why. Important
reasoning must survive in the PR or design docs even if commits are squashed.
Do not generate per-function documentation or copy the full specification into
every issue. Add dedicated decision records only when the existing document
becomes difficult to navigate.
