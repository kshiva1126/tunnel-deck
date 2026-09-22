#!/bin/sh

set -eu

workspace=${1:?workspace path is required}
control_root=${SYMPHONY_CONTROL_ROOT:?SYMPHONY_CONTROL_ROOT is required}
. "$control_root/scripts/symphony/common.sh"
become_agent_for_workspace "$workspace" "$0" "$@"

require_command git
require_command cargo

issue_number=$(issue_number_from_workspace "$workspace")
branch=$(branch_for_issue "$issue_number")

[ -d "$workspace" ] || die "workspace does not exist: $workspace"
[ -z "$(find "$workspace" -mindepth 1 -maxdepth 1 -print -quit)" ] || \
  die "new workspace is not empty: $workspace"

git clone --single-branch --branch "$base_branch" \
  "https://github.com/$repo_slug.git" "$workspace"
git -C "$workspace" switch -c "$branch"
cargo fetch --locked --manifest-path "$workspace/Cargo.toml"

printf 'Prepared %s for GitHub issue #%s\n' "$branch" "$issue_number"
