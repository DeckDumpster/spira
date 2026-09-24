#!/usr/bin/env bash
# gh-app.sh — gh wrapper that authenticates as the GitHub App via broker token.
# Set SPIRA_GH to this script's path in spira.conf to have the harness act as
# the App rather than as the operator's personal account.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
_tok="$("${SPIRA_BROKER_BIN}" token)" || {
    printf 'gh-app.sh: broker token failed\n' >&2; exit 1; }
exec env GH_TOKEN="$_tok" gh "$@"
