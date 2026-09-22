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
    "$SYMPHONY_CONTROL_ROOT/scripts/symphony/before_run.sh" "$PWD"
  after_run: |
    "$SYMPHONY_CONTROL_ROOT/scripts/symphony/after_run.sh" "$PWD"
  timeout_ms: 1200000

agent:
  max_concurrent_agents: 1
  max_turns: 1

codex:
  command: setpriv --reuid=$SYMPHONY_AGENT_UID --regid=$SYMPHONY_AGENT_GID --clear-groups env -u SSH_AUTH_SOCK -u SYMPHONY_GITHUB_TOKEN -u GH_TOKEN -u GITHUB_TOKEN PATH=/usr/local/cargo/bin:$PATH codex app-server
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
