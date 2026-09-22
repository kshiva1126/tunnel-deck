---
tracker:
  kind: github
  provider:
    repo: kshiva1126/tunnel-deck
    token: $SYMPHONY_GITHUB_TOKEN
  required_labels:
    - agent-ready
  active_states:
    - open
  terminal_states:
    - closed

polling:
  interval_ms: 30000

workspace:
  root: $SYMPHONY_WORKSPACE_ROOT

hooks:
  after_create: |
    "$SYMPHONY_CONTROL_ROOT/scripts/symphony/after_create.sh" "$PWD"
  before_run: |
    python3 "$SYMPHONY_CONTROL_ROOT/scripts/symphony/gate.py" before "$PWD"
  after_run: |
    python3 "$SYMPHONY_CONTROL_ROOT/scripts/symphony/gate.py" after "$PWD"
  timeout_ms: 1200000

agent:
  max_concurrent_agents: 1
  max_turns: 1

codex:
  # Shared host/Docker launcher pins the model; see docs/symphony.md.
  command: '"$SYMPHONY_CONTROL_ROOT/scripts/symphony/codex.sh"'
  approval_policy: never
  thread_sandbox: danger-full-access
  turn_sandbox_policy:
    type: dangerFullAccess
---

Work on GitHub Issue `{{ issue.identifier }}` in TunnelDeck.

Issue title: `{{ issue.title }}`

Issue body:

{{ issue.description }}

Follow `AGENTS.md`, `CONTRIBUTING.md`, `README.md`, and every document under
`docs/`. Before editing, inspect the relevant implementation and tests, then
briefly state the affected modules, intended behavior, and verification plan.

Keep the change limited to this issue. Do not change agreed product behavior,
storage or IPC compatibility, authentication policy, or architecture boundaries
without recording the blocker instead of guessing.

Implement the issue, add behavior-focused tests, and run:

1. `cargo fmt --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-features`

During the turn, update `.symphony-run-report.json` without changing its schema,
issue number, or run ID. Record a concise reviewable summary (not private
chain-of-thought): scope, facts, decisions and reasons, rejected alternatives,
entry points/state owner/external effects/failure cleanup, test commands and
results, unknowns or owner decisions, and the final status. Use `completed`,
`failed`, `blocked`, or `interrupted`; leave commit and pull request as null
because the trusted hook fills them. Never put credentials or sensitive
environment values in the report. The trusted hook validates it and upserts one
marked comment on the source Issue.

If all acceptance criteria are met and every required check passes, commit the
intended changes on the branch prepared by the harness. Use a focused commit
message. Do not push, create a pull request, change labels, or close the issue;
the host-side publishing hook handles those actions without exposing GitHub
credentials to the agent.

Remote Linux/macOS CI cannot run until that hook publishes the pull request.
Do not treat the absence of pre-publication remote CI as a blocker: when the
implementation is complete and the three local Rust checks above pass, commit
the work. Remote CI remains a human-review condition after publication.

If the work is incomplete or blocked, do not create a commit. Leave a concise
final explanation with the failing criterion or required owner decision.
