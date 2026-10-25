#!/bin/sh
# attempts.sh <bead> <want> — the attempt count the claims leave on <bead>.
# Exit 3 until <bead> is SUBMITTED, 0 when the count is <want>, 9 when it is not.
set -u
bead="$1"; want="$2"
[ "$(spira-lc state "$bead")" = SUBMITTED ] || exit 3
got="$(spira-claim attempts "$bead" --db "$SIM_BEADS_DB")" || { echo "attempts: spira-claim attempts failed" >&2; exit 9; }
[ "$got" = "$want" ] || { echo "attempts: $bead has $got attempt(s), wanted $want" >&2; exit 9; }
