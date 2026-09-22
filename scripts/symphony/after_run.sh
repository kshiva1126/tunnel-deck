#!/bin/sh

set -eu

workspace=${1:?workspace path is required}
control_root=${SYMPHONY_CONTROL_ROOT:?SYMPHONY_CONTROL_ROOT is required}
. "$control_root/scripts/symphony/common.sh"

trusted_git_read() {
  if [ "$(id -u)" -eq 0 ] && [ -n "${SYMPHONY_AGENT_UID:-}" ]; then
    setpriv --reuid="$SYMPHONY_AGENT_UID" --regid="$SYMPHONY_AGENT_GID" --clear-groups \
      env -u SYMPHONY_GITHUB_TOKEN -u GH_TOKEN -u GITHUB_TOKEN -u SSH_AUTH_SOCK \
      -u GIT_DIR -u GIT_WORK_TREE -u GIT_COMMON_DIR -u GIT_INDEX_FILE \
      -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES -u GIT_PREFIX \
      GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
      git -C "$workspace" "$@"
  else
    env -u SYMPHONY_GITHUB_TOKEN -u GH_TOKEN -u GITHUB_TOKEN -u SSH_AUTH_SOCK \
      -u GIT_DIR -u GIT_WORK_TREE -u GIT_COMMON_DIR -u GIT_INDEX_FILE \
      -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES -u GIT_PREFIX \
      GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
      git -C "$workspace" "$@"
  fi
}

# Docker keeps durable gate/review state root-owned. Publish as the workspace
# owner, then return to this trusted parent for review state and GitHub writes.
if [ "$(id -u)" -eq 0 ] && [ -n "$SYMPHONY_AGENT_UID" ] \
    && [ "${SYMPHONY_AUTO_REVIEW:-0}" = 1 ] \
    && [ "${SYMPHONY_VALIDATE_ONLY:-0}" != 1 ] \
    && [ "${SYMPHONY_TRUSTED_PUBLISH_ONLY:-0}" != 1 ]; then
  chown -R "$SYMPHONY_AGENT_UID:$SYMPHONY_AGENT_GID" "$workspace"
  validated_commit=$(trusted_git_read rev-parse HEAD)
  printf '%s\n' "$validated_commit" | grep -Eq '^[0-9a-f]{40}$' || \
    die "workspace HEAD is invalid before validation"
  set +e
  setpriv --reuid="$SYMPHONY_AGENT_UID" --regid="$SYMPHONY_AGENT_GID" --clear-groups \
    env -u SYMPHONY_GITHUB_TOKEN -u GH_TOKEN -u GITHUB_TOKEN -u SSH_AUTH_SOCK \
    SYMPHONY_VALIDATE_ONLY=1 SYMPHONY_VALIDATED_COMMIT="$validated_commit" \
    "$0" "$workspace"
  validate_status=$?
  set -e
  [ "$validate_status" -eq 0 ] || exit "$validate_status"
  after_validation=$(trusted_git_read rev-parse HEAD)
  [ "$after_validation" = "$validated_commit" ] || \
    die "workspace HEAD changed during validation"

  # Only the root-owned parent receives the token and performs remote writes.
  SYMPHONY_TRUSTED_PUBLISH_ONLY=1 SYMPHONY_VALIDATED_COMMIT="$validated_commit" \
    "$0" "$workspace"

  require_token
  require_command gh
  require_command python3
  issue_number=$(issue_number_from_workspace "$workspace")
  branch=$(branch_for_issue "$issue_number")
  pr_number=$(GH_TOKEN="$SYMPHONY_GITHUB_TOKEN" gh pr list --repo "$repo_slug" \
    --head "$branch" --state open --json number \
    --jq 'if length == 1 then .[0].number else error("expected one open PR") end') || \
    die "published PR could not be uniquely resolved"

  set +e
  python3 "$control_root/scripts/symphony/review.py" \
    "$workspace" "$issue_number" "$pr_number"
  review_status=$?
  set -e
  if [ "$review_status" -eq 2 ]; then
    printf 'Published pull request; automated review stopped for human review.\n'
    exit 0
  fi
  exit "$review_status"
fi

if [ "${SYMPHONY_TRUSTED_PUBLISH_ONLY:-0}" != 1 ]; then
  become_agent_for_workspace "$workspace" "$0" "$@"
fi

require_command git
require_command cargo
require_command python3
if [ "${SYMPHONY_VALIDATE_ONLY:-0}" != 1 ]; then
  require_token
  require_command gh
fi

agent_command() {
  env -u SYMPHONY_GITHUB_TOKEN -u GH_TOKEN -u GITHUB_TOKEN -u SSH_AUTH_SOCK "$@"
}

issue_number=$(issue_number_from_workspace "$workspace")
branch=$(branch_for_issue "$issue_number")
failure_status=failed
trusted_push_dir=

record_failure() {
  status=$?
  trap - EXIT HUP INT TERM
  if [ -n "$trusted_push_dir" ]; then
    rm -rf -- "$trusted_push_dir"
  fi
  if [ "$status" -ne 0 ]; then
    python3 - "$workspace/.symphony-run-report.json" "$failure_status" <<'PY' || true
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
if sys.argv[2] == "publish_failed" or report["status"] not in ("blocked", "interrupted"):
    report["status"] = sys.argv[2]
path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY
    python3 "$control_root/scripts/symphony/run_report.py" \
      publish "$workspace" "$issue_number" >/dev/null || true
  fi
  exit "$status"
}
if [ "${SYMPHONY_VALIDATE_ONLY:-0}" != 1 ]; then
  trap record_failure EXIT HUP INT TERM
  report_url=$(python3 "$control_root/scripts/symphony/run_report.py" \
    publish "$workspace" "$issue_number") || \
    die "run report publication failed; nothing was published"
fi

if [ "${SYMPHONY_TRUSTED_PUBLISH_ONLY:-0}" != 1 ]; then
  current_branch=$(trusted_git_read branch --show-current)
  [ "$current_branch" = "$branch" ] || \
    die "refusing to publish unexpected branch: $current_branch"
  [ -z "$(trusted_git_read status --porcelain)" ] || \
    die "agent left uncommitted changes; nothing was published"

  commit_count=$(trusted_git_read rev-list --count "origin/$base_branch..HEAD")
  [ "$commit_count" -gt 0 ] || die "agent created no commit; nothing was published"
  python3 "$control_root/scripts/symphony/run_report.py" \
    validate-complete "$workspace" "$issue_number"

  if trusted_git_read diff --no-ext-diff --no-textconv "origin/$base_branch...HEAD" | \
    grep -Eiq '(github_pat_[A-Za-z0-9_]{20,}|gh[opsur]_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{20,}|"(access|refresh|id)_token"[[:space:]]*:)'; then
    die "possible credential found in committed diff; nothing was published"
  fi

  (
    cd "$workspace"
    agent_command cargo fmt --check
    agent_command cargo clippy --all-targets --all-features -- -D warnings
    agent_command cargo test --all-features
  )
  validated_commit=${SYMPHONY_VALIDATED_COMMIT:-$(trusted_git_read rev-parse HEAD)}
  [ "$(trusted_git_read rev-parse HEAD)" = "$validated_commit" ] || \
    die "workspace HEAD changed during validation"
  [ "${SYMPHONY_VALIDATE_ONLY:-0}" != 1 ] || exit 0
fi

# Recheck after Rust checks, immediately before the first remote write.
python3 "$control_root/scripts/symphony/gate.py" verify "$workspace"

current_branch=$(trusted_git_read branch --show-current)
[ "$current_branch" = "$branch" ] || die "workspace branch changed before publication"
[ -z "$(trusted_git_read status --porcelain)" ] || die "workspace changed before publication"
published_commit=${SYMPHONY_VALIDATED_COMMIT:-$(trusted_git_read rev-parse HEAD)}
[ "$(trusted_git_read rev-parse HEAD)" = "$published_commit" ] || \
  die "workspace HEAD changed after validation"

failure_status=publish_failed
trusted_push_dir=$(mktemp -d "${SYMPHONY_STATE_ROOT:?}/trusted-push.XXXXXX")
chmod 0700 "$trusted_push_dir"
trusted_bundle="$trusted_push_dir/source.bundle"
# The trusted shell opens the root-owned destination. Git reads the agent-owned
# repository as the agent and inherits only that already-open output descriptor.
trusted_git_read bundle create - "$published_commit" >"$trusted_bundle"
chmod 0600 "$trusted_bundle"
env -u SYMPHONY_GITHUB_TOKEN -u GH_TOKEN -u GITHUB_TOKEN -u SSH_AUTH_SOCK \
  GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
  git init --bare "$trusted_push_dir" >/dev/null
env -u SYMPHONY_GITHUB_TOKEN -u GH_TOKEN -u GITHUB_TOKEN -u SSH_AUTH_SOCK \
  GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
  git -C "$trusted_push_dir" -c protocol.allow=never -c protocol.file.allow=always \
  fetch --no-tags "$trusted_bundle" "$published_commit" >/dev/null
[ "$(env -u SYMPHONY_GITHUB_TOKEN -u GH_TOKEN -u GITHUB_TOKEN -u SSH_AUTH_SOCK \
  GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
  git -C "$trusted_push_dir" rev-parse FETCH_HEAD)" = "$published_commit" ] || \
  die "trusted staging did not reproduce the validated commit"
env -u GIT_DIR -u GIT_WORK_TREE -u GIT_COMMON_DIR -u GIT_INDEX_FILE \
  -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES -u GIT_PREFIX \
  GIT_ASKPASS="$control_root/scripts/symphony/git-askpass.sh" \
  GIT_TERMINAL_PROMPT=0 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
  GH_TOKEN= GITHUB_TOKEN= SSH_AUTH_SOCK= \
  git -C "$trusted_push_dir" -c protocol.allow=never -c protocol.https.allow=always \
  -c core.hooksPath=/dev/null -c credential.helper= \
  push --no-verify "https://github.com/$repo_slug.git" \
  "$published_commit:refs/heads/$branch"
rm -rf -- "$trusted_push_dir"
trusted_push_dir=

issue_title=$(GH_TOKEN="$SYMPHONY_GITHUB_TOKEN" \
  gh issue view "$issue_number" --repo "$repo_slug" --json title --jq .title)

existing_pr=$(GH_TOKEN="$SYMPHONY_GITHUB_TOKEN" \
  gh pr list --repo "$repo_slug" --head "$branch" --state open \
  --json url --jq '.[0].url // empty')

if [ -n "$existing_pr" ]; then
  pr_url=$existing_pr
else
  body_file=$(mktemp)
  {
    printf 'GitHub Issue #%s を Symphony + Codex で実装しました。\n\n' "$issue_number"
    printf '実装判断と検証履歴: %s\n\n' "$report_url"
    printf '## 検証\n\n'
    printf '%s\n' "- \`cargo fmt --check\`"
    printf '%s\n' "- \`cargo clippy --all-targets --all-features -- -D warnings\`"
    printf '%s\n\n' "- \`cargo test --all-features\`"
    printf 'Refs #%s\n' "$issue_number"
  } >"$body_file"

  if ! pr_url=$(GH_TOKEN="$SYMPHONY_GITHUB_TOKEN" \
    gh pr create --repo "$repo_slug" --base "$base_branch" --head "$branch" \
    --title "$issue_title" --body-file "$body_file"); then
    rm -f "$body_file"
    false
  fi
  rm -f "$body_file"
fi

python3 - "$workspace/.symphony-run-report.json" "$pr_url" "$published_commit" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
report["pull_request"] = sys.argv[2]
report["commit"] = sys.argv[3]
report["status"] = "completed"
path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY
python3 "$control_root/scripts/symphony/run_report.py" \
  publish "$workspace" "$issue_number" >/dev/null

if [ "${SYMPHONY_TRUSTED_PUBLISH_ONLY:-0}" = 1 ]; then
  : # The root-owned caller continues with automated review.
elif [ "${SYMPHONY_AUTO_REVIEW:-0}" = 1 ]; then
  pr_number=${pr_url##*/}
  set +e
  python3 "$control_root/scripts/symphony/review.py" \
    "$workspace" "$issue_number" "$pr_number"
  review_status=$?
  set -e
  if [ "$review_status" -eq 2 ]; then
    trap - EXIT HUP INT TERM
    printf 'Published pull request; automated review stopped for human review: %s\n' "$pr_url"
    exit 0
  fi
  [ "$review_status" -eq 0 ] || false
else
  GH_TOKEN="$SYMPHONY_GITHUB_TOKEN" \
    gh issue edit "$issue_number" --repo "$repo_slug" \
    --remove-label agent-ready --add-label human-review
fi

trap - EXIT HUP INT TERM
printf 'Published pull request: %s\n' "$pr_url"
