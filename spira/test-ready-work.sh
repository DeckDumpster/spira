#!/usr/bin/env bash
#
# test-ready-work.sh — ready.sh's "ready work" row counts open plan beads through spira-lc.
#
# tier: T1
# covers: spira/ready.sh spira/ready-lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/ready-lib.sh"

BIN="$(mktemp -d)"
trap 'rm -rf "$BIN"' EXIT
export PATH="$BIN:$PATH"

stub() {   # stub <stdout> <exit>; records argv in $BIN/argv
    printf '#!/bin/sh\necho "$@" > "%s/argv"\nprintf %%s '"'"'%s'"'"'\nexit %s\n' "$BIN" "$1" "$2" > "$BIN/spira-lc"
    chmod +x "$BIN/spira-lc"
}

stub '{"items":[{"id":"sp-a"},{"id":"sp-b"}]}' 0
_out="$(ready_work nondefault-plan)"
want "two open plan beads print pass with 2" "pass  2 open plan bead(s)" "$_out"
want "the configured plan label is what is read" "--label nondefault-plan" "$(cat "$BIN/argv")"

stub '{"items":[]}' 0
want "no open beads warns" "WARN  no open plan work" "$(ready_work plan)"

stub 'boom' 1
want "a failing read is unknown, not a pass" "?     ready work" "$(ready_work plan)"
nowant "a failing read does not name the deleted binary" "sentinel" "$(ready_work plan)"

stub 'not json' 0
want "an unparseable answer is unknown" "?     ready work" "$(ready_work plan)"
