#!/bin/sh
# rc-ledger.sh — drive the verdict ledger through the round controller's store door and check each
# answer. A red identity is refused by its own hash and by its proven member on another selection;
# a transient fault backs its identity off; an identity that differs only in its base is admitted.
# Exit 0 when every answer is the expected one, 9 on the first that is not.
set -u
id="--repo sim --members rl-a@t1 --base rl-base-0 --image-digest rl-img-0 --harness-version rl-h"
other="--repo sim --members rl-b@t1 --base rl-base-0 --image-digest rl-img-0 --harness-version rl-h --selection all"

fail() { echo "rc-ledger: $1" >&2; exit 9; }
ask() {  # ask <expected line> <verb> <args...>
    want=$1
    shift
    out=$(round-controller ledger "$@" 2>&1) || fail "ledger $1 exited $?: $out"
    printf '%s\n' "$out" | grep -qx "$want" || fail "ledger $1: wanted the line '$want', got: $out"
}

# shellcheck disable=SC2086
{
    ask 'refuses=none' refuses $id --selection all
    ask 'attempt=1' record $id --selection all --batch rl-b1 --class red:build
    ask 'refuses=red-identity step=build' refuses $id --selection all
    ask 'refuses=none' refuses $id --selection some
    ask 'attempt=1' record $id --selection some --batch rl-b2 --class fault:vm
    ask 'attempt=2' record $id --selection some --batch rl-b2 --class fault:vm --evidence again
    ask 'refuses=none' refuses --repo sim --members rl-a@t1 --base rl-base-1 --image-digest rl-img-0 --harness-version rl-h --selection all
    ask 'refuses=none' refuses $other
}
out=$(round-controller ledger refuses $id --selection some 2>&1) || fail "refuses after faults exited $?: $out"
printf '%s\n' "$out" | grep -q '^refuses=backing-off until=' || fail "a vm fault did not back its identity off: $out"
exit 0
