#!/bin/sh

set -eu

script_dir=$(CDPATH='' cd -- "$(dirname "$0")" && pwd)
control_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)

command -v symphony >/dev/null 2>&1 || {
  printf 'symphony is not installed or not on PATH\n' >&2
  exit 1
}
command -v codex >/dev/null 2>&1 || {
  printf 'codex is not installed or not on PATH\n' >&2
  exit 1
}
command -v gh >/dev/null 2>&1 || {
  printf 'gh is not installed or not on PATH\n' >&2
  exit 1
}

command -v python3 >/dev/null 2>&1 || {
  printf 'python3 is required for trusted admission checks\n' >&2
  exit 1
}

if [ -z "${SYMPHONY_GITHUB_TOKEN:-}" ]; then
  SYMPHONY_GITHUB_TOKEN=$(gh auth token 2>/dev/null) || {
    printf 'Set SYMPHONY_GITHUB_TOKEN or log in with gh auth login\n' >&2
    exit 1
  }
  export SYMPHONY_GITHUB_TOKEN
fi

export SYMPHONY_CONTROL_ROOT="$control_root"
export SYMPHONY_WORKSPACE_ROOT="${SYMPHONY_WORKSPACE_ROOT:-$HOME/.local/share/symphony/tunnel-deck/workspaces}"

logs_root=${SYMPHONY_LOGS_ROOT:-$HOME/.local/state/symphony/tunnel-deck/logs}
port=${SYMPHONY_PORT:-4000}
export SYMPHONY_STATE_ROOT="${SYMPHONY_STATE_ROOT:-$HOME/.local/state/symphony/tunnel-deck/gates}"

mkdir -p "$SYMPHONY_WORKSPACE_ROOT" "$logs_root"

exec python3 "$script_dir/worker.py" symphony "$control_root/WORKFLOW.md" \
  --logs-root "$logs_root" \
  --port "$port" \
  --i-understand-that-this-will-be-running-without-the-usual-guardrails
