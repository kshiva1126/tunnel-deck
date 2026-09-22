#!/bin/sh

set -eu

workspace=${1:?workspace path is required}
control_root=${SYMPHONY_CONTROL_ROOT:?SYMPHONY_CONTROL_ROOT is required}
. "$control_root/scripts/symphony/common.sh"
become_agent_for_workspace "$workspace" "$0" "$@"

require_token
require_command git
require_command gh
require_command cargo
require_command python3

issue_number=$(issue_number_from_workspace "$workspace")
branch=$(branch_for_issue "$issue_number")
current_branch=$(git -C "$workspace" branch --show-current)
failure_status=failed

record_failure() {
  status=$?
  trap - EXIT HUP INT TERM
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
trap record_failure EXIT HUP INT TERM

report_url=$(python3 "$control_root/scripts/symphony/run_report.py" \
  publish "$workspace" "$issue_number") || \
  die "run report publication failed; nothing was published"

[ "$current_branch" = "$branch" ] || \
  die "refusing to publish unexpected branch: $current_branch"
[ -z "$(git -C "$workspace" status --porcelain)" ] || \
  die "agent left uncommitted changes; nothing was published"

commit_count=$(git -C "$workspace" rev-list --count "origin/$base_branch..HEAD")
[ "$commit_count" -gt 0 ] || die "agent created no commit; nothing was published"
python3 "$control_root/scripts/symphony/run_report.py" \
  validate-complete "$workspace" "$issue_number"

if git -C "$workspace" diff --no-ext-diff "origin/$base_branch...HEAD" | \
  grep -Eiq '(github_pat_[A-Za-z0-9_]{20,}|gh[opsur]_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{20,}|"(access|refresh|id)_token"[[:space:]]*:)'; then
  die "possible credential found in committed diff; nothing was published"
fi

(
  cd "$workspace"
  cargo fmt --check
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --all-features
)

# Recheck after Rust checks, immediately before the first remote write.
python3 "$control_root/scripts/symphony/gate.py" verify "$workspace"

failure_status=publish_failed
GIT_ASKPASS="$control_root/scripts/symphony/git-askpass.sh" \
GIT_TERMINAL_PROMPT=0 \
git -C "$workspace" push --set-upstream origin "$branch"

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

python3 - "$workspace/.symphony-run-report.json" "$pr_url" <<'PY'
import json
import pathlib
import subprocess
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
report["pull_request"] = sys.argv[2]
report["commit"] = subprocess.run(
    ["git", "-C", str(path.parent), "rev-parse", "HEAD"],
    check=True, text=True, stdout=subprocess.PIPE).stdout.strip()
report["status"] = "completed"
path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY
python3 "$control_root/scripts/symphony/run_report.py" \
  publish "$workspace" "$issue_number" >/dev/null

GH_TOKEN="$SYMPHONY_GITHUB_TOKEN" \
  gh issue edit "$issue_number" --repo "$repo_slug" \
  --remove-label agent-ready --add-label human-review

trap - EXIT HUP INT TERM
printf 'Published pull request: %s\n' "$pr_url"
