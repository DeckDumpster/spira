#!/bin/sh
# slain-gap.sh <bead> — between a slain session and the next claim, the bead is READY and no attempt is charged.
# Exit 3 outside that gap (not one session run yet, or not READY), 0 when it held, 9 when an attempt is charged.
set -u
bead="$1"
[ "$(grep -c "\"bead\":\"$bead\"" "$SIM_WORLD/agent-state/briefs.jsonl" 2>/dev/null || echo 0)" = 1 ] || exit 3
[ "$(spira-lc state "$bead")" = READY ] || exit 3
got="$(spira-claim attempts "$bead" --db "$SIM_BEADS_DB")" || { echo "slain-gap: spira-claim attempts failed" >&2; exit 9; }
[ "$got" = 0 ] || { echo "slain-gap: $bead has $got attempt(s) after a slain release, wanted 0" >&2; exit 9; }
