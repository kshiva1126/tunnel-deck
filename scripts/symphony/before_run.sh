#!/bin/sh

set -eu

workspace=${1:?workspace path is required}
control_root=${SYMPHONY_CONTROL_ROOT:?SYMPHONY_CONTROL_ROOT is required}
. "$control_root/scripts/symphony/common.sh"
become_agent_for_workspace "$workspace" "$0" "$@"

require_token
require_command git
require_command cargo
require_command codex

issue_number=$(issue_number_from_workspace "$workspace")
expected_branch=$(branch_for_issue "$issue_number")

if ! git -C "$workspace" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  [ -z "$(find "$workspace" -mindepth 1 -maxdepth 1 -print -quit)" ] || \
    die "workspace is not an empty or valid Git worktree: $workspace"
  rmdir "$workspace" || die "cannot replace stale workspace: $workspace"
  mkdir "$workspace"
  "$control_root/scripts/symphony/after_create.sh" "$workspace"
fi

current_branch=$(git -C "$workspace" branch --show-current)
[ "$current_branch" = "$expected_branch" ] || \
  die "expected branch $expected_branch, found $current_branch"

origin_url=$(git -C "$workspace" remote get-url origin)
case "$origin_url" in
  "https://github.com/$repo_slug"|"https://github.com/$repo_slug.git") ;;
  *) die "unexpected origin remote: $origin_url" ;;
esac

[ "$current_branch" != "$base_branch" ] || die "refusing to run on $base_branch"

printf 'Preflight passed for GitHub issue #%s on %s\n' \
  "$issue_number" "$current_branch"
