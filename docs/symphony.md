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

## Run in Docker (recommended)

Docker limits the worker to the checked-out repository, dedicated workspace and
log directories, and temporary credential mounts. The container drops Linux
capabilities except those needed to start Codex under the unprivileged worker
UID, uses a read-only root filesystem, and exposes the dashboard only on
loopback. It does not receive the Docker socket. The Codex turn uses the Docker
container itself as its filesystem boundary.

```sh
./scripts/symphony/start-docker.sh
```

If the current login session predates addition to the `docker` group, the
script uses `sg docker` for this run. Log out and back in once to make ordinary
`docker` commands work without that compatibility step.

The first run builds a local image with pinned Symphony, Codex, GitHub CLI, and
Rust versions. It mounts the host Codex login read-only, copies it into a
temporary in-container home, and deletes the mounted GitHub token immediately
after Symphony receives it. Supplying a repository-scoped
`SYMPHONY_GITHUB_TOKEN` remains
the least-privilege option.

## Run directly on the host

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

The worker accepts one issue at a time. In Docker mode, Codex runs as an
unprivileged UID inside the read-only container and can write to the dedicated
workspace and cache mounts. GitHub credentials are removed from the Codex
process and are held by the root-owned Symphony process. The trusted hooks
retain the credential so they can clone,
push the prepared branch, open a pull request, and move the issue from
`agent-ready` to `human-review`.

Review the diff, CI, and acceptance criteria before merging. The generated pull
request uses `Refs #N`, so merge does not automatically close an issue whose
full acceptance criteria still need manual confirmation.
