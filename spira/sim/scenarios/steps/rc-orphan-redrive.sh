#!/bin/sh
# rc-orphan-redrive.sh write|check — `write` puts one orphan into the world's control store
# through the round controller's door (`spira-lc rc`): a CI_RUNNING round whose last lease is held
# by no one, and a round never leased, which is waiting for a slot and no orphan. `check` runs one
# more `round-controller pass` over them and exits 0 only when that pass names the orphan as held
# on its old runner (the binary has no VM records, so no runner is shown gone) and the store
# shows nothing re-driven: no attempt, lease or slot, and the waiting round untouched. 9, naming
# what it saw, otherwise.
set -u
fail() { echo "rc-orphan-redrive: $1" >&2; exit 9; }

get() { spira-lc rc get --json "$(printf '{"key":"%s"}' "$1")" 2>&1; }

put() {  # put <key> <data JSON>
    body=$(printf '{"txn":{"compares":[],"ops":[{"put":["%s",{"data":%s}]}],"else_ops":[]},"request":{"id":"rc-orphan-redrive-%s","actor":"rc-orphan-redrive","kind":"seed"}}' "$1" "$2" "$(printf '%s' "$1" | tr / -)")
    out=$(spira-lc rc txn --json "$body" 2>&1) || fail "put $1: $out"
    printf '%s' "$out" | grep -q '"succeeded":true' || fail "put $1 did not commit: $out"
}

round() {  # round <id> <member> [<extra JSON fields>]
    put "batches/sim/$1" "$(printf '{"batch_id":"%s","repo":"sim","state":"CiRunning","head":"od-h-%s","base":"od-base-0","pass":1,"version":1,"tested_base":"od-base-0","members":[{"id":"%s","tip":"t1"}]%s}' "$1" "$1" "$2" "${3:-}")"
}

case "${1:-}" in
write)
    put lanes/sim '{"landing_ref":"od-base-0","image_digest":"od-img-0","harness_version":"od-harness","concurrency":4}'
    round od-orphan od-a ',"last_lease_token":{"main":1,"sub":0}'
    round od-waiting od-b ;;
check)
    pass=$(round-controller pass 2>&1) || fail "the pass failed: $pass"
    printf '%s\n' "$pass" | grep 'prior-runner-alive' | grep -q 'batch=od-orphan' || fail "the pass did not hold the orphan on its old runner: $pass"
    printf '%s\n' "$pass" | grep -q 'batch=od-waiting' && fail "the pass named a round that was never leased: $pass"
    for key in batches/sim/od-orphan/redrive batches/sim/od-waiting/redrive; do
        out=$(get "$key")
        printf '%s' "$out" | grep -q '"ok":null' || fail "$key was written: $out"
    done
    locks=$(spira-lc rc list --json '{"prefixes":["locks/"],"selector":"everything"}' 2>&1)
    printf '%s' "$locks" | grep -q '"items":\[\]' || fail "the pass took a lease or a slot: $locks"
    out=$(get batches/sim/od-waiting)
    printf '%s' "$out" | grep -q '"last_lease_token"' && fail "the waiting round was leased: $out" ;;
*)
    fail "usage: rc-orphan-redrive.sh write|check" ;;
esac
exit 0
