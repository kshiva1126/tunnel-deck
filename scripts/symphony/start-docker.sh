#!/bin/sh

set -eu

script_dir=$(CDPATH='' cd -- "$(dirname "$0")" && pwd)
control_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
script_path="$control_root/scripts/symphony/start-docker.sh"
host_uid=${SYMPHONY_HOST_UID:-$(id -u)}
host_gid=${SYMPHONY_HOST_GID:-$(id -g)}
export SYMPHONY_HOST_UID="$host_uid" SYMPHONY_HOST_GID="$host_gid"

command -v docker >/dev/null 2>&1 || {
  printf 'docker is not installed or not on PATH\n' >&2
  exit 1
}
command -v gh >/dev/null 2>&1 || {
  printf 'gh is not installed or not on PATH\n' >&2
  exit 1
}

if ! docker info >/dev/null 2>&1; then
  if getent group docker | cut -d: -f4 | tr ',' '\n' | grep -Fxq "$(id -un)"; then
    printf 'Refreshing docker group membership for this command. Re-login to make it permanent.\n' >&2
    exec sg docker -c "$script_path"
  fi
  printf 'Cannot access the Docker daemon without sudo\n' >&2
  exit 1
fi

auth_file=${CODEX_AUTH_FILE:-$HOME/.codex/auth.json}
[ -r "$auth_file" ] || {
  printf 'Codex authentication file is not readable: %s\n' "$auth_file" >&2
  exit 1
}

token_file=$(mktemp)
chmod 0600 "$token_file"

if [ -n "${SYMPHONY_GITHUB_TOKEN:-}" ]; then
  printf '%s' "$SYMPHONY_GITHUB_TOKEN" >"$token_file"
else
  gh auth token >"$token_file"
fi

image=${SYMPHONY_DOCKER_IMAGE:-tunnel-deck-symphony:local}
workspace_root=${SYMPHONY_WORKSPACE_ROOT:-$HOME/.local/share/symphony/tunnel-deck/workspaces}
logs_root=${SYMPHONY_LOGS_ROOT:-$HOME/.local/state/symphony/tunnel-deck/logs}
cargo_home=${SYMPHONY_CARGO_HOME:-$HOME/.cache/symphony/tunnel-deck/cargo}
port=${SYMPHONY_PORT:-4000}
container_name=tunnel-deck-symphony

# Invoked through the signal/exit trap below.
# shellcheck disable=SC2329
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  docker stop -t 5 "$container_name" >/dev/null 2>&1 || true
  rm -f "$token_file"
  exit "$status"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$workspace_root" "$logs_root" "$cargo_home"

docker build \
  --build-arg "WORKER_UID=$host_uid" \
  --build-arg "WORKER_GID=$host_gid" \
  --tag "$image" \
  --file "$control_root/docker/symphony/Dockerfile" \
  "$control_root/docker/symphony"

container_id=$(docker run --detach --rm --init \
  --name "$container_name" \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --pids-limit 512 \
  --tmpfs /tmp:rw,exec,nosuid,nodev,size=1g \
  --tmpfs "/home/worker/.codex:rw,nosuid,nodev,uid=$host_uid,gid=$host_gid,mode=0700" \
  --publish "127.0.0.1:$port:4001" \
  --mount "type=bind,src=$control_root,dst=/control,readonly" \
  --mount "type=bind,src=$workspace_root,dst=/workspaces" \
  --mount "type=bind,src=$logs_root,dst=/logs" \
  --mount "type=bind,src=$cargo_home,dst=/home/worker/.cargo" \
  --mount "type=bind,src=$auth_file,dst=/run/secrets/codex-auth.json,readonly" \
  --mount "type=bind,src=$token_file,dst=/run/secrets/github-token,readonly" \
  "$image" \
  sh -eu -c '
    cp /run/secrets/codex-auth.json "$HOME/.codex/auth.json"
    chmod 0600 "$HOME/.codex/auth.json"
    SYMPHONY_GITHUB_TOKEN=$(cat /run/secrets/github-token)
    export SYMPHONY_GITHUB_TOKEN
    socat TCP-LISTEN:4001,fork,reuseaddr TCP:127.0.0.1:4000 &
    exec symphony /control/WORKFLOW.md \
      --logs-root /logs \
      --port 4000 \
      --i-understand-that-this-will-be-running-without-the-usual-guardrails
  ')

printf 'Symphony dashboard: http://127.0.0.1:%s/\n' "$port"
docker logs --follow "$container_id" &
logs_pid=$!
container_status=$(docker wait "$container_id")
wait "$logs_pid" 2>/dev/null || true
exit "$container_status"
