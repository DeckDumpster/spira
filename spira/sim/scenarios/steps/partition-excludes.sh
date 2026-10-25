#!/bin/sh
# partition-excludes.sh <plain> <excluded> — the sentinel's builder summons claim <plain> and never <excluded>.
# Exit 3 until <plain> is claimed, 0 while only it has been, 9 once <excluded> is claimed or still not READY.
set -u
plain="$1"; excluded="$2"
fail() { echo "partition-excludes: $1" >&2; exit 9; }
log="$SIM_WORLD/claim-order"
[ -s "$log" ] || exit 3
[ "$(cut -d' ' -f1 "$log" | sort -u | tr '\n' ' ')" = "$plain " ] || fail "claimed beads other than $plain: $(tr '\n' ';' < "$log")"
[ "$(spira-lc state "$excluded")" = READY ] || fail "$excluded is not READY: $(spira-lc state "$excluded")"
