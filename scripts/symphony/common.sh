#!/bin/sh

set -eu

# These constants are consumed by scripts that source this file.
# shellcheck disable=SC2034
repo_slug="kshiva1126/tunnel-deck"
# shellcheck disable=SC2034
base_branch="main"

die() {
  printf 'symphony harness: %s\n' "$*" >&2
  exit 1
}

issue_number_from_workspace() {
  workspace_name=$(basename "$1")
  case "$workspace_name" in
    GH-[0-9]*) printf '%s\n' "${workspace_name#GH-}" ;;
    *) die "workspace name must be GH-<issue-number>: $workspace_name" ;;
  esac
}

branch_for_issue() {
  printf 'symphony/issue-%s\n' "$1"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command is missing: $1"
}

require_token() {
  [ -n "${SYMPHONY_GITHUB_TOKEN:-}" ] || die "SYMPHONY_GITHUB_TOKEN is not set"
}
