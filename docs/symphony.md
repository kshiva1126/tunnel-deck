# Symphony GitHub Issue workflow

This repository can run an opt-in local Symphony worker for GitHub Issues.
Symphony watches open issues carrying the `agent-ready` label, creates an
isolated workspace and branch, and starts Codex after trusted admission checks. A host-side
hook publishes a pull request only after the agent has committed its work and
all required Rust checks pass. The same trusted hook then monitors required
GitHub Actions and CodeRabbit, performs bounded remediation on that one PR, and
squash-merges the exact verified head in the ordinary success case.

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

The generated pull request uses `Refs #N`. The trusted review driver closes the
Issue explicitly only after the merge and final acceptance revalidation; a
stopped or partial result remains open with `human-review`.

## Automated review, remediation, and merge (GH-28)

[`scripts/symphony/review.py`](../scripts/symphony/review.py) is the state owner
after PR publication. Both launchers set `SYMPHONY_AUTO_REVIEW=1`; the ordinary
test hook can leave it unset to exercise publication alone. The driver accepts
only an open PR in `kshiva1126/tunnel-deck` whose base is `main`, head is
`symphony/issue-N`, body links the same Issue, and head repository is not a
fork. Initial admission still rejects **all** PR history and therefore cannot
replay old work: only the trusted post-publication call carries the PR number
into review mode.

In Docker, `after_run.sh` publishes and verifies the workspace as the unprivileged
workspace owner, then returns to its root-owned trusted parent for `/state` and
GitHub operations. Codex and all workspace commands are explicitly dropped back
to the configured agent UID/GID; the remediation context is made readable only
to that identity. Before each push, that identity serializes the validated
commit to a bundle through a file descriptor opened by the trusted parent. The
parent imports only that bundle into root-owned temporary Git metadata; it never
opens the agent-controlled repository with root Git. Pushes then use the fixed
repository URL, disable repository hooks and credential helpers, and obtain the
token only through the trusted askpass helper. This keeps agent-controlled Git
configuration outside the credential boundary while preserving root-owned
retry state.

The required check names default to `ubuntu-latest,macos-latest,CodeRabbit`
(the current CI matrix job names) and may be changed
as one comma-separated trusted launcher setting in `SYMPHONY_REQUIRED_CHECKS`.
Missing, queued, or running checks wait; a completed result other than success,
neutral, or skipped is a failure. CodeRabbit review threads are fetched through
GitHub GraphQL. Pagination beyond the bounded first 100 threads/comments fails
closed instead of silently overlooking a finding.

For a failed check, only the check name and bounded check-run title/summary are
included. For CodeRabbit, only the unresolved finding body, path, line, and
commit SHA are included. This JSON is explicitly marked untrusted, capped at
48 KiB, scanned for credential-like data, written mode 0600, and removed after
the turn. `codex exec` receives that file and the existing workspace, but its
environment has `SYMPHONY_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, and
`SSH_AUTH_SOCK` removed. Codex never pushes or calls GitHub: the trusted parent
requires a new clean commit, runs the three local Rust checks, scans the whole
PR diff for credentials, and pushes to the existing branch.

The default bounds are three remediation commits and two hours total, polling
every 30 seconds. Trusted operators may set `SYMPHONY_REMEDIATION_ATTEMPTS`,
`SYMPHONY_REVIEW_SECONDS`, and `SYMPHONY_REVIEW_POLL_SECONDS`; host and Docker
use the same code and defaults. Start time, PR identity, attempt count, and the
last failure fingerprint are stored mode 0600 beside the existing trusted gate
records, so a hook/process restart cannot reset either bound. An identical
failure fingerprint on the next head stops immediately, so a repeated external
effect is not attempted. The review record is removed only after a verified
merge and Issue close; manual recovery may remove it only while the worker is
stopped and after inspecting the still-open PR.

Immediately before merge the driver re-fetches the PR identity and exact head
SHA, native Issue dependencies, every required check, and unresolved review
threads. It sends squash merge with that verified SHA, re-fetches the PR to
prove a merge commit exists, and only then closes the source Issue. A new head,
failed check, dependency, unresolved thread, malformed/partial response,
timeout, permission failure, or unknown merge result fails closed. Dependency,
Cargo supply-chain, architecture, IPC/storage/process-policy, host-key weakening,
destructive-migration, and credential-like changes stop at `human-review`
instead of being auto-merged.

Cargo manifest/lock changes are not automatically supply-chain exceptions.
The trusted review driver fetches `Cargo.lock` and, when needed, `Cargo.toml`
from the exact PR base and head commits through GitHub. Each file must be a
regular, base64-encoded UTF-8 response no larger than 2 MiB. It compares the
lock package identity set `(name, version, source, checksum)` and each package's
normalized dependency references. Formatting and dependency ordering may differ,
while a dependency-edge change, package addition/removal, or any identity-field
change stops at `human-review`.

For `Cargo.toml`, the base text must remain unchanged and in order. The only
permitted inserted lines are unique, single-line registry dependency
declarations such as `libc = "0.2"` inside dependency, dev-dependency,
build-dependency, or target-specific dependency tables, and every declared
package name must already occur in the head lock. Table-style declarations and
all other manifest edits stop, including features, alternate source/registry,
git/path dependencies, patches, profiles, package build settings, removals,
and version edits. Missing, empty, oversized, non-UTF-8, malformed, duplicate,
unfetchable, or otherwise ambiguous Cargo inputs fail closed. This narrow
exception does not change the existing stops for `docs/architecture.md`,
authentication, storage/IPC/process-policy paths, dangerous SSH settings,
destructive migration, dependencies, required CI, or CodeRabbit findings.

### Stops and manual recovery

The driver moves exceptional work out of `agent-ready`, adds `human-review`,
and exits nonzero. This includes exhausted attempt/time bounds, a repeated
finding, API/auth/schema ambiguity, mismatched branch/repository/SHA/state,
open dependencies, risky policy boundaries, unsafe diagnostic content, failed
local validation, or an unverifiable merge. The source Issue's single marked
run-report comment remains the review and recovery record; do not delete PR
history to re-admit it through the initial gate.

An operator should inspect the current PR head, required checks, review threads,
dependencies, and the sanitized hook diagnostic. Resolve the cause on the
existing PR or record the owner decision on the Issue. If automation is to be
resumed, stop the worker first, preserve the workspace, clear only the matching
trusted stop/permit state as described below, restore the intended queue label,
and restart the same launcher. Never bypass the exact-SHA final revalidation.

## Decision report on the source Issue (GH-26)

Each admitted turn starts with a private, untracked
`.symphony-run-report.json` in its workspace. Codex updates this structured
file with a review-oriented summary: scope, discovered facts, selected
decisions and reasons, rejected alternatives, execution ownership and cleanup,
verification, remaining risks, and final outcome. It must not contain private
chain-of-thought, credentials, or unnecessary environment values. The trusted
hook, not Codex, owns GitHub access and fills in the final commit and pull
request links.

The hook creates or updates one source-Issue comment beginning with
`<!-- tunnel-deck-symphony-run-report -->`. A retry replaces its own marked
attempt section and a later run appends one history section; it never rewrites
the Issue body. The generated PR links back to this comment so human reviewers
and CodeRabbit can recover the implementation context. The comment is only a
summary and link index: accepted product and architecture decisions remain
authoritative in the existing documents under `docs/`, while code and test
results remain the evidence for implemented behavior.

The report validator requires the exact schema, matching positive Issue
number, bounded report/comment sizes, known outcome, and valid repository-local
PR URL and commit ID. It rejects malformed input and credential-like strings
before any comment write. Comment lookup is paginated and marker matches are
restricted to comments owned by the authenticated trusted publisher; foreign
markers are ignored. Zero trusted matches creates the comment, one updates it,
and multiple trusted matches fail closed for operator inspection. Writes are
restricted to comments on the admitted source Issue.

An initial `interrupted` report exists before Codex starts, so an unconditional
after hook can publish useful context even if the turn ends early. Test or
workspace validation failures become `failed`; failures after validation and
during remote publication become `publish_failed`. A Codex-reported `blocked`
outcome is retained. Comment failures are logged, make the trusted hook fail,
and enter the existing durable stop flow; the hook makes only one bounded
retry from its failure cleanup and never rewrites code or the Issue body.
Because both host and Docker launchers use the same trusted hooks, their report
behavior is identical. The four sensitive variables continue to be removed
before the Codex child starts.

If comment publication fails, inspect the sanitized hook diagnostic and the
trusted `GH-N.stopped` record with the worker stopped. Repair permission,
timeout, duplicate-marker, or report-validation problems before following the
normal recovery procedure below. A timeout has an unknown remote outcome, so
inspect the Issue before retrying; the marker and run ID make a retry
idempotent.

## Codex model selection (GH-25)

`WORKFLOW.md`'s `codex.command` invokes the trusted
[`scripts/symphony/codex.sh`](../scripts/symphony/codex.sh) in both launch modes.
That script is the single model setting and starts:

```sh
codex app-server -c 'model="gpt-5.6-sol"'
```

The explicit CLI configuration overrides a configured or recommended model;
see the [OpenAI configuration documentation](https://developers.openai.com/codex/config-advanced/).
We choose Sol for the quality/usage balance of this workflow, rather than
following future recommendation changes. No `model_reasoning_effort` override
is added: without an explicit user configuration, Sol uses its model default.
Host installations still honor an explicitly configured reasoning effort.
The Docker launcher copies only Codex authentication, not the host config.
GitHub credential removal and Docker's UID/GID drop remain unchanged.

To change the model later, edit the argument in the **trusted control
checkout's** `scripts/symphony/codex.sh`, update the harness expectation and
this section, then restart the worker for new sessions. Editing an issue
workspace does not change the running worker's `/control` checkout. Existing
sessions are not evidence that the updated launcher has taken effect.

After a new Symphony turn, safely verify its Codex rollout record with the
following command, replacing the path with that session's JSONL file under
`$CODEX_HOME/sessions` (or `~/.codex/sessions`). In Docker, run the check inside
the running container before stopping it: the Codex home is temporary.
The check prints only a fixed success/failure message, never the session
contents, prompt, environment, or credentials. Do not attach full session or
configuration dumps to verification evidence.

```sh
python3 - /path/to/new-session.jsonl <<'PY'
import json
import sys

try:
    models = []
    with open(sys.argv[1]) as session:
        for line in session:
            event = json.loads(line)
            if event.get("type") == "turn_context":
                models.append(event["payload"].get("model"))
    valid = bool(models) and all(model == "gpt-5.6-sol" for model in models)
except (OSError, ValueError, KeyError, TypeError, AttributeError):
    valid = False
print("Verified session model: gpt-5.6-sol" if valid else "Session model not verified")
sys.exit(0 if valid else 1)
PY
```

Record the session identifier, Codex version, launch mode, and check result.
The offline harness executes `WORKFLOW.md`'s actual command in host and
simulated Docker modes, checking the exact model argument, absence of a
reasoning override, and removal of all four sensitive environment variables.
This verifies command forwarding, not live model availability or a real
Symphony session; the new-session check above supplies that separate evidence.

GH-25 local verification (2026-09-22): on Linux, all 33 harness tests and
`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
and `cargo test --all-features` passed (47 unit tests and 3 CLI tests).
The new command test first failed in both modes without the model argument,
then passed with it. The actual configured command also started Codex 0.155.1;
its `config/read` response confirmed `model = "gpt-5.6-sol"`. Only that model
confirmation was printed; no inference turn or GitHub operation was requested.
Shell syntax and diff whitespace checks passed.

Deployment acceptance evidence must come from a new **Symphony-dispatched**
session using the updated trusted control checkout. Record that evidence on
GH-25 after merging and restarting the worker; a run from the issue workspace
itself still uses the pre-change control checkout. Native Docker isolation and
macOS are covered by the remote Linux/macOS checks after publication.

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
Response validation enforces a fixed 100-level JSON nesting limit before
decoding, independently of the Python runtime's recursion limit. Exceeding it
persists a stop record so a malformed response cannot cause repeated API calls
on retries.
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
These rules also apply to label-update timeouts, whose remote outcome may be
unknown. The local stop is written before either request; do not infer that a
timed-out request left GitHub unchanged. Inspect the actual labels during
manual recovery.

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

Label-timeout verification (2026-09-22): all 30 harness tests and the three
required Rust checks passed on Linux, Python 3.11.2 / Rust 1.85.0. New coverage
uses actual subprocess timeouts for both label removal and addition in host
and simulated Docker modes. It verifies that the stop exists before the label
request, removal timeout requests shutdown, addition timeout preserves queue
removal, and redispatch/restart cannot run Codex or publication or repeat API
calls. Existing production code satisfies these cases; this follow-up adds
tests and recovery clarification. Native GitHub E2E, Docker runtime isolation,
and native macOS were not run; remote CI remains a post-publication review
condition.

Cross-version JSON-depth verification (2026-09-22): all 32 harness tests and
the three required Rust checks passed on Linux, Python 3.11.2 / Rust 1.85.0.
The fixed pre-decode limit removes reliance on Python's changing decoder
recursion behavior; boundary coverage also verifies that brackets inside JSON
strings do not count as nesting. Native GitHub E2E and Docker runtime isolation
were not rerun. The prior remote Linux/macOS jobs exposed this portability gap;
replacement jobs are required before merge.

GH-26 local verification (2026-09-22): on Linux, all 40 offline harness tests
passed in host and simulated Docker command modes. Coverage includes initial
comment creation, update and same-run deduplication, every terminal report
status, malformed/wrong-Issue/oversized/credential-like report rejection,
workspace and publication failures, bounded comment API failure, PR backlink,
and continued removal of GitHub credentials from Codex. Shell syntax, Python
compilation, and diff whitespace checks passed. With `/usr/local/cargo/bin` on
`PATH`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D
warnings`, and `cargo test --all-features` passed (47 unit tests, 3 CLI tests,
and doc-tests). The first test invocation reached all Rust tests but could not
start rustdoc until that documented toolchain PATH was restored. No live
GitHub writes, native Docker isolation, or native macOS run was performed;
remote Linux/macOS CI remains a post-publication review condition.
