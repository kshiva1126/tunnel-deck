# Symphony GitHub Issue workflow

This repository can run an opt-in local Symphony worker for GitHub Issues.
Symphony watches open issues carrying the `agent-ready` label, creates an
isolated workspace and branch, and starts Codex after trusted admission checks. A host-side
hook publishes a pull request only after the agent has committed its work and
all required Rust checks pass.

## One-time setup

Install `symphony`, `codex`, `gh`, and Python 3.9+, then authenticate Codex and GitHub. The
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

## Dependency admission and replay protection (GH-24)

Both launchers now require Python 3.9+ (included in the Docker image) and use
`worker.py` to supervise Symphony. `WORKFLOW.md` calls trusted `gate.py` before
and after a turn. The existing shell hooks still own workspace preparation,
Rust validation, and publication; they are invoked only after admission.
Missing or non-executable trusted shell hooks also persist a stop record and
remove the issue from the queue. Repair the trusted installation before using
the same manual recovery procedure; a hook launch error must not become an
unbounded retry. Diagnostics omit OS exception text and installation paths.
The TunnelDeck application, storage/IPC contracts, and SSH policy are unchanged.

The gate reads the target issue, every page of its native
[`dependencies/blocked_by` REST relationship](https://docs.github.com/en/rest/issues/issue-dependencies#list-dependencies-an-issue-is-blocked-by),
and every page of PR history for `symphony/issue-N`. It uses `gh api
--paginate --slurp`, validates the JSON structure and dependency states, and
limits each API command, including pagination, to 30 seconds. Issue body text
such as `Depends on` is not an admission source. A merged dependency PR does
not resolve a dependency while its issue remains open.

Admission requires an open issue with `agent-ready`, without `blocked` or
`human-review`, all dependencies closed, and no PR history on the prepared
branch. An open, closed, or merged PR on that branch requires human review;
automatic reruns do not update previously published PRs. This deliberately
trades automatic PR follow-up for protection against replaying merged work.
API errors, inaccessible dependencies, missing permissions, timeouts, unknown
states, and invalid/incomplete responses all refuse admission. Validation
also covers missing or empty `SYMPHONY_GITHUB_TOKEN`: the gate never falls back
to `GH_TOKEN`, `GITHUB_TOKEN`, or CLI-stored credentials. It persists a stop and
requests worker shutdown because it cannot remove `agent-ready` without the
trusted credential. Restore that credential before following recovery below.
Response validation
includes JSON nesting beyond the decoder's limit: this also persists a stop
record so a malformed response cannot cause repeated API calls on retries.
Duplicate JSON object fields are also rejected rather than accepting the last
value: conflicting issue or dependency states must never silently permit a run.
Non-JSON constants (`NaN`, `Infinity`, and `-Infinity`) are rejected even in
otherwise unused fields, so Python's permissive decoder cannot admit an invalid
API response. These failures use the same durable stop and recovery procedure.
Validation also rejects non-integer issue numbers, including JSON floating-point values
that compare equal to the requested number. Diagnostics
contain fixed reasons and validated dependency repository/issue identifiers,
never issue titles/bodies, API response text, CLI stderr, or token values.

On refusal, the trusted gate persists `GH-N.stopped` **before** attempting any
label update. It removes `agent-ready`, then adds `blocked`, without posting
comments. A later attempt sees the local stop record and performs no GitHub
calls, Codex launch, or publish-hook invocation. That redispatch also writes
`halt` to stop the worker: a hook failure by itself does not stop Symphony's
retry scheduler. This covers stale tracker snapshots and manually restoring
`agent-ready` without clearing the stop record, including after publication.
If adding `blocked` fails,
removing `agent-ready` still leaves the issue out of the queue. If removing
`agent-ready` fails, the gate also writes `halt`; the supervisor checks it once
per second and terminates Symphony's process group (SIGTERM, then SIGKILL after
5 seconds). Restart is refused until an operator clears the halt. This stops
the scheduler even when GitHub cannot accept the transition. Always use the
launchers: invoking the Symphony binary directly bypasses this shutdown guard.

Symphony v0.0.3 invokes `after_run` even when `before_run` fails. The gate
therefore creates a one-use `GH-N.permit` only after the before hook succeeds,
and consumes it before invoking the after hook. Without it, the after gate
returns without calling the publish shell script. The after gate rechecks
admission; the publish shell script checks again after Rust validation,
immediately before push. A successful publication leaves a local stop record
as well. These checks are snapshots: use one worker per state directory and
repository, and do not run a competing publisher or merge the same branch
concurrently with publication. GitHub does not offer an atomic transaction
covering dependency inspection, push, and PR creation.

State is outside the agent workspace, with directory mode 0700 and files 0600:

| Mode | Default trusted state directory |
| --- | --- |
| Host | `~/.local/state/symphony/tunnel-deck/gates` |
| Docker | `~/.local/state/symphony/tunnel-deck/docker-gates`, mounted at `/state` |

`SYMPHONY_STATE_ROOT` overrides the host path in either launcher. Use a dedicated
absolute directory, not the workspace or control checkout. Docker makes that
directory root-owned; the Codex UID cannot read or change admission records.
Hooks load code from the read-only `/control` mount and query GitHub before
handing off to workspace hooks. `codex.sh` removes `SYMPHONY_GITHUB_TOKEN`,
`GH_TOKEN`, `GITHUB_TOKEN`, and `SSH_AUTH_SOCK` in both modes; Docker also drops
the UID/GID with `setpriv`. Direct host mode retains its existing same-user
trust limitation: environment removal is not OS isolation from that user's
credential store or other processes. No GitHub credential is passed to Codex
by this workflow.

## Recover and requeue

There is no automatic resume on dependency closure; removing and re-adding a
label alone does not clear the durable stop. For a stopped issue:

1. Stop the worker. Inspect `GH-N.stopped` and, if present, `halt` in the trusted
   state directory. Docker records require a host administrator to read them.
2. Resolve every native blocking issue, or repair GitHub access/timeouts. Check
   the target issue and all PR states for `symphony/issue-N`. For already merged
   or published work, review acceptance criteria and close the issue manually
   when appropriate; track additional work in a new issue. Do not delete PR
   history or reuse a merged branch to bypass admission.
3. Inspect the existing workspace and preserve any unfinished commits. For a
   legitimate retry with no PR history, remove `blocked` and restore
   `agent-ready`. Use the read-only `verify` command below to confirm eligibility.
4. While the worker remains stopped, remove only that issue's `GH-N.stopped`
   and `GH-N.permit` from the trusted directory. If a global halt occurred,
   repair access when needed and inspect its reason. Remove `agent-ready` from
   stopped issues that should remain paused before clearing `halt`; restore it
   only for eligible issues whose stop records are intentionally cleared.
   Use administrator privileges for Docker's root-owned records. Never clear
   state while a worker is running.
5. Restart the same launcher with the same state directory. Admission queries
   GitHub again; unresolved dependencies or another error stop the issue again.

State survives workspace deletion, worker restarts, and Docker container
replacement. Keep it when upgrading the trusted control checkout. After
upgrading from an older workflow, existing branch PR history and issue labels
are checked even when no local stop record exists.

## Verification and safe native-dependency E2E

Run the offline harness on Linux and macOS:

```sh
python3 -m unittest discover -s tests/symphony -v
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

The harness substitutes GitHub, Git, and Codex commands, uses temporary
workspaces, and exercises the actual gate and shell hooks. Success paths run
the Rust publish checks on a tiny local fixture crate. Cases cover multi-page
responses, open/closed dependencies, API and schema failures, actual subprocess
timeout, replay/merge rejection, pre-push rechecking, one-use admission, token
removal, failed label transitions, durable retry suppression, shutdown on
redispatch of a stopped issue, manual recovery, dependencies becoming open or
the dependency API failing/returning malformed JSON during an admitted turn,
and supervisor shutdown. Recovery coverage follows the complete path in both
host and simulated Docker modes: closure and relabeling alone remain stopped;
clearing the stop while the worker is down permits fresh checks and exactly one
publication; subsequent dispatch cannot launch Codex or publish again.
`setpriv` is simulated: those tests verify dispatch
parity, not native Docker UID isolation. CI runs this harness on both OSes.

For a safe check against **real GitHub dependencies**, an owner can use
throwaway issues in `kshiva1126/tunnel-deck` while all workers are stopped:

1. Create target issue T and blocker issues A/B. Give T `agent-ready`, with no
   `blocked`/`human-review` or PR history. In GitHub's Relationships UI, mark T
   as **blocked by** A/B (not merely a body reference). Keep the workers stopped
   throughout so these test issues cannot be dispatched.
2. Set `SYMPHONY_GITHUB_TOKEN` using a read-only token with Issues and Pull
   requests read access. Run the following, replacing `123` with T's number:

   ```sh
   python3 scripts/symphony/gate.py verify /tmp/GH-123
   ```

   `verify` performs reads only: no workspace creation, labels, comments, Codex,
   Git pushes, or publication. Exit 1 means refused; exit 0 means eligible.
   Refusals print a sanitized reason, including open dependency identifiers.
3. Repeat with no relationships (expect 0), A/B open (1), just A closed (1),
   and both closed (0). Confirm a merged PR for A while A stays open still
   refuses T. Remove read permissions or unset the token and expect 1. Run
   `verify` for a previously published/merged issue and expect 1.
4. Repeat the same read-only command in the built Docker image with the same
   token and trusted checkout:

   ```sh
   docker run --rm --read-only --env SYMPHONY_GITHUB_TOKEN \
     --mount "type=bind,src=$PWD,dst=/control,readonly" \
     tunnel-deck-symphony:local \
     python3 /control/scripts/symphony/gate.py verify /tmp/GH-123
   ```

   The outcomes and open dependency identifiers must agree. This command
   deliberately runs only the verifier; it never starts Symphony or Codex.
5. Remove `agent-ready` from the throwaway issues and close them before
   restarting a worker. Record issue URLs, native relationship states,
   exit statuses, OS, and tool versions, without recording credentials.

This read-only E2E validates the real dependency API separately from the
harness's launch/publish side-effect assertions. Native GitHub E2E, Docker
runtime isolation, and remote Linux/macOS CI are not claimed by local fake
results; remote CI remains a human-review condition after publication.

GH-24 local verification (2026-09-22): Linux x86_64, Python 3.11.2, Rust 1.85.0.
All 22 harness tests, `cargo fmt --check`, Clippy with `-D warnings`, and
`cargo test --all-features` passed (47 unit tests, 3 CLI tests, doc-tests).
The first Rust test run could not find `rustdoc`; rerunning with
`/usr/local/cargo/bin` on PATH passed. Shell syntax and `git diff --check` also
passed. Follow-up verification also covers worker shutdown when a stopped
issue is dispatched again; all three Rust checks passed again with the full
toolchain PATH. Additional tests cover invalid issue numbers and page shapes,
and verify that the E2E verifier makes only GET requests and writes no local
admission state on either success or refusal.
Hook-launch failure follow-up: all 23 harness tests and the three required
Rust checks passed on the same Linux environment. Missing and non-executable
before/after hooks now have regression coverage for durable stop records,
consumed permits, sanitized diagnostics, and retries without external effects.
No live GitHub writes, native dependency E2E, Docker runtime test, or
remote Linux/macOS CI were run in this agent workspace.

JSON decoder failure follow-up: all 24 harness tests and the three required
Rust checks passed on the same Linux environment. Excessively nested responses
from the issue, dependency, and PR endpoints reproduced repeated API calls
before the fix. They now create stop records, suppress Codex and publication,
and halt redispatch without repeated external calls or response-text disclosure.
The native E2E and platform verification limits above still apply.

Duplicate-field follow-up: all 25 harness tests and the three required Rust
checks passed on the same Linux environment. A conflicting duplicate issue
state reproduced admission before the fix. Issue and dependency responses with
duplicate fields now persist a stop record, suppress Codex/publication, and
halt redispatch without further API calls. No live GitHub writes or native E2E
were performed; the platform verification limits above still apply.

Non-JSON constant follow-up: all 26 harness tests and the three required Rust
checks passed on Linux (2026-09-22). The regression first reproduced Codex
admission for an issue response containing `NaN`; the fix rejects all three
non-JSON constants across issue, dependency, and PR responses, with durable
stops and no external calls on redispatch. Real GitHub E2E, Docker runtime
isolation, and native macOS remain unverified; their procedures and
post-publication review requirements above are unchanged.

During-turn revalidation follow-up (2026-09-22): all 27 harness tests and the
three required Rust checks passed on Linux. The new regression confirms that
after a successful admission and Codex launch, an open dependency or unusable
dependency response consumes the permit, persists a stop, and prevents the
publish hook from running. Repeated after hooks and redispatch make no further
external calls; redispatch requests worker shutdown. Existing production code
already satisfies these cases, so this follow-up changes tests and evidence
only. Native GitHub E2E, Docker runtime isolation, and native macOS were not run.

Missing-credential verification (2026-09-22): all 28 harness tests and the
three required Rust checks passed on Linux. The added regression covers absent
and empty trusted tokens in host and simulated Docker command modes, with
`GH_TOKEN` and `GITHUB_TOKEN` still set. Both refuse admission, persist a stop,
halt redispatch and worker restart, and invoke no GitHub, Git, Codex, or publish
commands. Existing production code already satisfies this behavior; this
follow-up adds regression coverage and documents recovery. Native dependency
E2E, Docker runtime isolation, and remote Linux/macOS CI remain unverified.

Recovery publication verification (2026-09-22): all 28 harness tests and the
three required Rust checks passed on Linux. The existing recovery test now
continues through Codex launch, the real publish hook with fake Git/GitHub,
permit consumption, and replay suppression in both command modes. Production
behavior is unchanged. Native GitHub E2E, Docker isolation, and macOS were not
run; remote CI remains a human-review condition after publication.

Pre-push revalidation verification (2026-09-22): all 29 harness tests and the
three required Rust checks passed on Linux. New coverage changes the fake API
response only on the final pre-push query, after before/after admission and the
publish hook's Rust checks. An open dependency, dependency API error, malformed
JSON, or merged branch PR prevents push and PR creation in host and simulated
Docker modes. The after gate consumes the permit and persists a stop; repeated
hooks neither launch Codex nor make external calls, and redispatch requests
worker shutdown. Existing production behavior is unchanged. Native GitHub E2E,
Docker runtime isolation, and native macOS were not run; remote CI remains a
post-publication human-review condition.
