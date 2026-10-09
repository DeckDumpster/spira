#!/usr/bin/env bash
#
# loom.sh — source the harness configuration, then start the Loom server.
#
# Systemd execs this so the binary reads its configuration from environment variables that
# conf.sh sets: SPIRA_LC_BIN, SPIRA_LOOM_ADDR, SPIRA_PATH and friends. A unit with a hardcoded
# SPIRA_LC_BIN works on exactly one box; a wrapper that sources conf.sh works on any.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

command -v loom >/dev/null 2>&1 || { printf 'loom: not found on PATH (%s)\n' "$PATH" >&2; exit 2; }

_conf_file="$(spira_toml_write_target)"   # the layer an operator edits, in the one source
_conf_mtime_0="$(stat --format='%Y' "$_conf_file" 2>/dev/null || echo 0)"
_tick="${SPIRA_LOOM_TICK:-5}"

loom "$@" &
_loom_pid=$!

trap '
    kill "$_loom_pid" 2>/dev/null || true
    wait "$_loom_pid" 2>/dev/null || true
    exit 0
' INT TERM HUP

while kill -0 "$_loom_pid" 2>/dev/null; do
    sleep "$_tick"
    if conf_changed "$_conf_file" "$_conf_mtime_0"; then
        printf 'loom.sh: config changed — exiting for restart\n' >&2
        kill "$_loom_pid" 2>/dev/null || true
        wait "$_loom_pid" 2>/dev/null || true
        exit 0
    fi
done
wait "$_loom_pid"
