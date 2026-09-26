#!/usr/bin/env bash
#
# test-timeout-lint.sh — timeout-lint.sh catches a bare numeric timeout/wall literal in
# aeon.sh, sentinel.sh or landing.sh, and does not fire on the poll-tick idiom those files
# already use.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION. A lint that reports the shipped tree clean is
# indistinguishable from one whose matcher never fires; a literal is planted in a scratch
# copy first, and only a lint that names it makes the shipped tree's silence mean anything
# (law-absence-needs-a-positive-control).
#
# defect: sp-eibeu
# tier: T1
# covers: spira/timeout-lint.sh spira/aeon.sh spira/sentinel.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-timeout-lint.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
lint() { bash "$HERE/timeout-lint.sh" "$@"; }

# ---------------------------------------------------------------------------------------
# THE POSITIVE CONTROL — a planted literal must be named, by file and line.
# ---------------------------------------------------------------------------------------
PLANT="$TMP/planted.sh"
printf '#!/usr/bin/env bash\nsleep 45\ntimeout 120 bash -c true\nTimeoutStartSec=600\n' > "$PLANT"

out="$(lint --scan "$PLANT")"
want "SEEN RED: a bare sleep literal is caught"          "sleep 45"          "$out"
want "and a bare timeout literal is caught"               "timeout 120"       "$out"
want "and a bare TimeoutStartSec is caught"                "TimeoutStartSec=600" "$out"
want "and the file:line is named"                          "$PLANT:2:"        "$out"

out="$(lint "$PLANT")"; rc=$?
is   "whole-file mode also refuses the plant" "1" "$rc"

# ---------------------------------------------------------------------------------------
# THE EXEMPTION IS EXACT AND SCOPED — the poll-tick idiom does not fire, and a named
# config key with an inline default does not fire either.
# ---------------------------------------------------------------------------------------
CLEAN="$TMP/clean.sh"
printf '#!/usr/bin/env bash\nwhile [ "$w" -lt "$grace" ]; do sleep 1; w=$((w+1)); done\nsleep "${FAYTH_HEARTBEAT_SECONDS:-30}"\ntimeout "$FAYTH_TIMEOUT_SECONDS" cmd\n' > "$CLEAN"
is   "the poll-tick idiom (sleep 1) is not flagged"        "" "$(lint --scan "$CLEAN")"

out="$(lint "$CLEAN")"; rc=$?
is   "and whole-file mode passes it" "0" "$rc"
want "and says how many files it checked" "clean" "$out"

# Withdrawn, and only now is a green reading evidence of anything.
rm -f "$PLANT"

# ---------------------------------------------------------------------------------------
# THE SHIPPED TREE. Read through the control above, this now means something.
# ---------------------------------------------------------------------------------------
out="$(lint)"; rc=$?
is   "GREEN AFTER: aeon.sh, sentinel.sh and landing.sh pass by default" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"

tl_summary
