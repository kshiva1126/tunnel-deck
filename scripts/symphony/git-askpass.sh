#!/bin/sh

set -eu

case "${1:-}" in
  *sername*) printf '%s\n' 'x-access-token' ;;
  *assword*) printf '%s\n' "${SYMPHONY_GITHUB_TOKEN:?SYMPHONY_GITHUB_TOKEN is required}" ;;
  *) exit 1 ;;
esac
