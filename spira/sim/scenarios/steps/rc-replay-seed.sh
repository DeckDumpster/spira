#!/bin/sh
# rc-replay-seed.sh [no-driver] — write the 10-31 replay's one-shard slice into the world's
# control store through the round controller's door (`spira-lc rc`): the sim lane; five ended
# passes of the repeat-red member at one tip, with the Image fault their Judge left standing; a
# parent left in ATTRIBUTING under a hand driver's lease (left out under `no-driver`), with three
# GREEN children stacked on it; and one stale record, ended with no pass. Exit 0 when every write
# committed, 9 on the first that did not.
set -u
fail() { echo "rc-replay-seed: $1" >&2; exit 9; }
n=0

put() {  # put <key> <data JSON>
    n=$((n + 1))
    body=$(printf '{"txn":{"compares":[],"ops":[{"put":["%s",{"data":%s}]}],"else_ops":[]},"request":{"id":"rc-replay-seed-%s","actor":"rc-replay","kind":"seed"}}' "$1" "$2" "$n")
    out=$(spira-lc rc txn --json "$body" 2>&1) || fail "put $1: $out"
    printf '%s' "$out" | grep -q '"succeeded":true' || fail "put $1 did not commit: $out"
}

round() {  # round <id> <state> <head> <tested_base> <member> [<extra JSON fields>]
    put "batches/sim/$1" "$(printf '{"batch_id":"%s","repo":"sim","state":"%s","head":"%s","base":"rp-base-0","pass":1,"version":1,"tested_base":"%s","members":[{"id":"%s","tip":"t1"}]%s}' "$1" "$2" "$3" "$4" "$5" "${6:-}")"
}

put lanes/sim '{"landing_ref":"rp-base-0","image_digest":"rp-img-0","harness_version":"rp-harness","concurrency":4}'

for k in 1 2 3 4 5; do
    round "rp-pk-$k" Abandoned "rp-pk-h$k" rp-base-0 rp-pk
done
put faults/sim/rp-base-0/rp-img-0 '{"cause":{"cause":"image","digest":"rp-img-0"},"evidence":"base run on rp-img-0: red","since":0}'

# The parent's last runner lapsed: its record names that lease, which no lock holds any more.
round rp-parent Attributing rp-parent-h rp-base-0 rp-parent ',"last_lease_token":{"main":1,"sub":0}'
round rp-child-1 Green rp-child-1-h rp-parent-h rp-c1 ',"parent":"rp-parent"'
round rp-child-2 Green rp-child-2-h rp-child-1-h rp-c2 ',"parent":"rp-child-1"'
round rp-child-3 Green rp-child-3-h rp-child-2-h rp-c3 ',"parent":"rp-child-2"'

# The hand driver takes the parent's lease as `release round-recert` does: the lock, then the
# record naming it and its holder.
if [ "${1:-}" != no-driver ]; then
    lock='{"resource":"round/sim/rp-parent","ttl_ms":36000000,"request":{"id":"rc-replay-seed-driver","actor":"round-recert","kind":"seed"}}'
    out=$(spira-lc rc lock --json "$lock" 2>&1) || fail "the hand driver's lease: $out"
    token=$(printf '%s' "$out" | sed -n 's/.*"token":\({[^}]*}\).*/\1/p')
    [ -n "$token" ] || fail "the hand driver's lease names no token: $out"
    round rp-parent Attributing rp-parent-h rp-base-0 rp-parent ",\"last_lease_token\":$token,\"lease_holder\":\"round-recert\""
fi

round rp-stale Abandoned rp-stale-h rp-base-0 rp-stale
exit 0
