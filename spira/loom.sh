#!/usr/bin/env bash
#
# loom.sh — source the harness configuration, then start the Loom server.
#
# Systemd execs this so the binary reads its configuration from environment variables that
# conf.sh sets: SPIRA_LC_BIN, SPIRA_LOOM_ADDR, SPIRA_PATH and friends. A unit with a hardcoded
# SPIRA_LC_BIN works on exactly one box; a wrapper that sources conf.sh works on any.
set -uo pipefail

_ts() { date -Is; }
_loom_pid=""
_stop_on() {
    printf 'loom.sh: %s SIG%s received — stopping loom\n' "$(_ts)" "$1" >&2
    local pid="${_loom_pid:-${!:-}}"
    if [ -n "$pid" ]; then
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
    exit 0
}
trap '_stop_on INT' INT
trap '_stop_on TERM' TERM
trap '_stop_on HUP' HUP

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

command -v loom >/dev/null 2>&1 || { printf 'loom: not found on PATH (%s)\n' "$PATH" >&2; exit 2; }

_conf_file="$(spira_toml_write_target)"   # the layer an operator edits, in the one source
_conf_stamp_0="$(conf_stamp "$_conf_file")"
_pinned_0="$(loom --print-config 2>&1)"
_tick="${SPIRA_LOOM_TICK:-5}"

loom "$@" &
_loom_pid=$!

# Loom pins only what --print-config prints; a write to any other key leaves it running.
_pinned_diff() {
    local now rc=0
    now="$(loom --print-config 2>&1)" || rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'config no longer resolves: %s' "$now"
        return 0
    fi
    [ "$now" = "$_pinned_0" ] && return 1
    local line old="" new=""
    while IFS= read -r line; do case $'\n'"$now"$'\n' in *$'\n'"$line"$'\n'*) ;; *) old="$old${old:+ }$line" ;; esac; done <<<"$_pinned_0"
    while IFS= read -r line; do case $'\n'"$_pinned_0"$'\n' in *$'\n'"$line"$'\n'*) ;; *) new="$new${new:+ }$line" ;; esac; done <<<"$now"
    printf '%s → %s' "$old" "$new"
}

while kill -0 "$_loom_pid" 2>/dev/null; do
    sleep "$_tick"
    if conf_changed "$_conf_file" "$_conf_stamp_0"; then
        if _why="$(_pinned_diff)"; then
            case "$_why" in
                "config no longer resolves:"*) printf 'loom.sh: %s %s — exiting for restart\n' "$(_ts)" "$_why" >&2 ;;
                *) printf 'loom.sh: %s config changed (%s) — exiting for restart\n' "$(_ts)" "$_why" >&2 ;;
            esac
            kill "$_loom_pid" 2>/dev/null || true
            wait "$_loom_pid" 2>/dev/null || true
            exit 0
        fi
        _conf_stamp_0="$(conf_stamp "$_conf_file")"
    fi
done
wait "$_loom_pid"
_rc=$?
printf 'loom.sh: %s loom exited rc=%s\n' "$(_ts)" "$_rc" >&2
exit "$_rc"
