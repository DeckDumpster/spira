#!/bin/sh
# claim-rank.sh <first> <second> — the sentinel's calls claim in spira-claim's rank, under a lease.
# <first> is the better-ranked bead. Exit 3 until both are claimed, 0 once they are, 9 on a wrong order or lease.
set -u
first="$1"; second="$2"
fail() { echo "claim-rank: $1" >&2; exit 9; }
log="$SIM_WORLD/claim-order"
[ "$(wc -l < "$log" 2>/dev/null || echo 0)" -ge 2 ] || exit 3
[ "$(sed -n 1p "$log" | cut -d' ' -f1)" = "$first" ] || fail "$first was not claimed first: $(tr '\n' ';' < "$log")"
[ "$(sed -n 2p "$log" | cut -d' ' -f1)" = "$second" ] || fail "$second was not claimed second: $(tr '\n' ';' < "$log")"
while read -r bead holder lease rune; do
    [ "$rune" = builder ] || fail "$bead was claimed for $rune"
    [ "$lease" -gt "$SPIRA_NOW" ] || fail "$bead lease $lease is not after the world's clock $SPIRA_NOW"
    [ "$lease" -le $((SPIRA_NOW + 3600)) ] || fail "$bead lease $lease is more than an hour past the world's clock $SPIRA_NOW"
done < "$log"
