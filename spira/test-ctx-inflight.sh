#!/usr/bin/env bash
#
# test-ctx-inflight.sh — the work-in-flight segment on the status line: what it shows when work
# is running, when nothing is, and when the snapshot cannot be read.
#
#   ./test-ctx-inflight.sh
#
# Every absence case first proves the same code path renders presence
# (law-absence-needs-a-positive-control), and runs under `env -i` with a frozen clock.
#
# tier: T1
# covers: spira/ctx-meter.sh cockpit-collect/src/probes/inflight.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "no [$3] in [$2]" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1" "found [$3] in [$2]" ;; *) ok "$1" ;; esac; }
strip() { sed 's/\x1b\[[0-9;]*m//g'; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/home" "$T/run" "$T/projects/-a-project"
EPOCH=1750000000
SNAP="$T/run/cockpit.env"

line() {
    tl_config SPIRA_RUN="$T/run" SPIRA_TOKEN_PROJECTS="$T/projects"
    printf '{"session_id":"s-1","cwd":"/tmp","context_window":{"total_input_tokens":1200,"current_usage":{"input_tokens":1200}}}' |
        env -i HOME="$T/home" PATH="$PATH" SPIRA_CONF="$T/no-such.conf" SPIRA_TOML="$SPIRA_TOML" \
            SPIRA_NOW="$EPOCH" ctx-meter.sh 2>/dev/null | strip
}
snap() { printf "SP_INFLIGHT_SEG='%s'\n" "$1" > "$SNAP"; touch -d "@$((EPOCH - ${2:-1}))" "$SNAP"; }

snap "⚙ 1 dwarf · 1 round (412/475) · 1 gate"
has "work running shows every kind with progress" "$(line)" "⚙ 1 dwarf · 1 round (412/475) · 1 gate"

snap "⚙ idle"
has "idle shows idle" "$(line)" "⚙ idle"

snap "⚙ ?rounds"
has "an unreadable source shows its ?" "$(line)" "?rounds"
hasnt "an unreadable source is not idle" "$(line)" "idle"

snap "⚙ idle" 600
L="$(line)"
has "a stale snapshot is ?inflight" "$L" "?inflight"
hasnt "a stale snapshot is not idle" "$L" "idle"

rm -f "$SNAP"
L="$(line)"
has "a missing snapshot is ?inflight" "$L" "?inflight"
hasnt "a missing snapshot is not idle" "$L" "idle"

printf "SP_OPEN='1'\n" > "$SNAP"; touch -d "@$EPOCH" "$SNAP"
has "a snapshot without the key is ?inflight" "$(line)" "?inflight"

echo
tl_summary
