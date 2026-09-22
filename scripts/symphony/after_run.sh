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

issue_number=$(issue_number_from_workspace "$workspace")
branch=$(branch_for_issue "$issue_number")
current_branch=$(git -C "$workspace" branch --show-current)

[ "$current_branch" = "$branch" ] || \
  die "refusing to publish unexpected branch: $current_branch"
[ -z "$(git -C "$workspace" status --porcelain)" ] || \
  die "agent left uncommitted changes; nothing was published"

commit_count=$(git -C "$workspace" rev-list --count "origin/$base_branch..HEAD")
[ "$commit_count" -gt 0 ] || die "agent created no commit; nothing was published"

if git -C "$workspace" diff --no-ext-diff "origin/$base_branch...HEAD" | \
  grep -Eiq '(github_pat_|gh[opsu]_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{20,}|"(access|refresh|id)_token"[[:space:]]*:)'; then
  die "possible credential found in committed diff; nothing was published"
fi

(
  cd "$workspace"
  cargo fmt --check
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --all-features
)

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
  trap 'rm -f "$body_file"' EXIT HUP INT TERM
  {
    printf 'GitHub Issue #%s を Symphony + Codex で実装しました。\n\n' "$issue_number"
    printf '## 検証\n\n'
    printf '%s\n' "- \`cargo fmt --check\`"
    printf '%s\n' "- \`cargo clippy --all-targets --all-features -- -D warnings\`"
    printf '%s\n\n' "- \`cargo test --all-features\`"
    printf 'Refs #%s\n' "$issue_number"
  } >"$body_file"

  pr_url=$(GH_TOKEN="$SYMPHONY_GITHUB_TOKEN" \
    gh pr create --repo "$repo_slug" --base "$base_branch" --head "$branch" \
    --title "$issue_title" --body-file "$body_file")
fi

GH_TOKEN="$SYMPHONY_GITHUB_TOKEN" \
  gh issue edit "$issue_number" --repo "$repo_slug" \
  --remove-label agent-ready --add-label human-review

printf 'Published pull request: %s\n' "$pr_url"
