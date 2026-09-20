#!/usr/bin/env bash
#
# loom.sh — source the harness configuration, then start the Loom server.
#
# Systemd execs this so the binary reads its configuration from environment variables that
# conf.sh sets: SPIRA_DB, SPIRA_LOOM_ADDR, SPIRA_PATH and friends. A unit with a hardcoded
# SPIRA_DB works on exactly one box; a wrapper that sources conf.sh works on any.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

if [ ! -x "${SPIRA_LOOM_BIN:-}" ]; then
    printf 'loom: binary not found at %s\n' "${SPIRA_LOOM_BIN:-<unset>}" >&2
    printf 'loom: build it: cd %s/loom && cargo build --release\n' "$SPIRA_REPO" >&2
    exit 2
fi

_conf_file="${SPIRA_CONF_FILE:-}"
_conf_mtime_0="$(stat --format='%Y' "$_conf_file" 2>/dev/null || echo 0)"
_tick="${SPIRA_LOOM_TICK:-5}"

"$SPIRA_LOOM_BIN" "$@" &
_loom_pid=$!

trap '
    kill "$_loom_pid" 2>/dev/null || true
    wait "$_loom_pid" 2>/dev/null || true
    exit 0
' INT TERM HUP

while kill -0 "$_loom_pid" 2>/dev/null; do
    sleep "$_tick"
    if [ -n "$_conf_file" ]; then
        _conf_mtime_now="$(stat --format='%Y' "$_conf_file" 2>/dev/null || echo 0)"
        if [ "$_conf_mtime_now" != "$_conf_mtime_0" ]; then
            printf 'loom.sh: config changed — exiting for restart\n' >&2
            kill "$_loom_pid" 2>/dev/null || true
            wait "$_loom_pid" 2>/dev/null || true
            exit 0
        fi
    fi
done
wait "$_loom_pid"
