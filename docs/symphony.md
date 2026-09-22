# Symphony GitHub Issue workflow

This repository can run an opt-in local Symphony worker for GitHub Issues.
Symphony watches open issues carrying the `agent-ready` label, creates an
isolated workspace and branch, and starts Codex in that workspace. A host-side
hook publishes a pull request only after the agent has committed its work and
all required Rust checks pass.

## One-time setup

Install `symphony`, `codex`, and `gh`, then authenticate Codex and GitHub. The
GitHub token needs repository access to `kshiva1126/tunnel-deck` with these
fine-grained permissions:

- Metadata: read
- Contents: read and write
- Issues: read and write
- Pull requests: read and write

Keep the token outside this repository. Either export it for the Symphony
process:

```sh
export SYMPHONY_GITHUB_TOKEN=github_pat_...
```

or log in with `gh auth login`; `scripts/symphony/start.sh` can obtain the token
from the GitHub CLI credential store.

Create these repository labels before the first run:

- `agent-ready`: eligible for Symphony dispatch
- `human-review`: pull request created and awaiting review
- `blocked`: owner input or an unresolved dependency is required

## Run

Add `agent-ready` only to a bounded, dependency-ready issue. Then run:

```sh
./scripts/symphony/start.sh
```

The dashboard is available at <http://127.0.0.1:4000/> by default. Override
the port with `SYMPHONY_PORT`. Workspaces and logs default to:

```text
~/.local/share/symphony/tunnel-deck/workspaces
~/.local/state/symphony/tunnel-deck/logs
```

The worker accepts one issue at a time. Codex has write access only to the issue
workspace and has no network access. GitHub credentials are removed from the
Codex process. The trusted host hooks retain the credential so they can clone,
push the prepared branch, open a pull request, and move the issue from
`agent-ready` to `human-review`.

Review the diff, CI, and acceptance criteria before merging. The generated pull
request uses `Refs #N`, so merge does not automatically close an issue whose
full acceptance criteria still need manual confirmation.
