#!/bin/sh
# rc-replay-check.sh <expectation> — read the replay slice back from the world's control store once
# the round controller's passes have run over it, and hold it to the replay expectation of the
# same name. Exit 0 when it holds, 9 naming what the store shows when it does not.
set -u
fail() { echo "rc-replay-check $1: $2" >&2; exit 9; }

get() {  # get <key>: the wire answer for the key
    spira-lc rc get --json "$(printf '{"key":"%s"}' "$1")" 2>&1
}

state_is() {  # state_is <round> <state>
    out=$(get "batches/sim/$1")
    printf '%s' "$out" | grep -q "\"state\":\"$2\"" || fail "$name" "$1 is not $2: $out"
}

absent() {  # absent <key>
    out=$(get "$1")
    printf '%s' "$out" | grep -q '"ok":null' || fail "$name" "$1 exists: $out"
}

name="$1"
case "$name" in
no_green_round_abandoned)
    for c in rp-child-1 rp-child-2 rp-child-3; do state_is "$c" Green; done ;;
stacked_parent_wastes_at_most_one_pass)
    for c in rp-child-1 rp-child-2 rp-child-3; do
        out=$(get "batches/sim/$c/status")
        printf '%s' "$out" | grep -q '"blocker":"stacked-on-unlanded"' || fail "$name" "$c is not held stacked on its unlanded parent: $out"
    done ;;
driver_recut_wastes_at_most_one_pass)
    state_is rp-parent Attributing
    absent batches/sim/rp-parent/redrive
    out=$(get batches/sim/rp-child-1/status)
    printf '%s' "$out" | grep -q '"parent_leased":true' || fail "$name" "the Lander did not see the hand driver's lease on rp-parent: $out" ;;
nothing_stacks_on_the_stale_record)
    state_is rp-stale Abandoned
    absent batches/sim/rp-stale/status ;;
pokebot_one_image_fault)
    out=$(get faults/sim/rp-base-0/rp-img-0)
    printf '%s' "$out" | grep -q '"digest":"rp-img-0"' || fail "$name" "the Image fault no longer stands: $out" ;;
pokebot_no_eject)
    for n in 1 2 3 4 5; do state_is "rp-pk-$n" Abandoned; absent "batches/sim/rp-pk-$n/redrive"; done ;;
*)
    fail "$name" "no such expectation" ;;
esac
exit 0
