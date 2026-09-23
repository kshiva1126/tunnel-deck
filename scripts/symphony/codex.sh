#!/bin/sh
set -eu

# Both launch modes use the same credential-free command. Docker additionally
# drops to the worker UID; direct host execution keeps the invoking UID.
if [ -n "${SYMPHONY_AGENT_UID:-}" ]; then
  set -- setpriv --reuid="$SYMPHONY_AGENT_UID" --regid="${SYMPHONY_AGENT_GID:?}" --clear-groups
else
  set --
fi
exec "$@" env -u SSH_AUTH_SOCK -u SYMPHONY_GITHUB_TOKEN -u GH_TOKEN -u GITHUB_TOKEN \
  PATH="/usr/local/cargo/bin:$PATH" codex app-server -c 'model="gpt-6-sol"'
