#!/usr/bin/env bash
#
# test-strand-throttle.sh — strand.sh suppresses 'starved' while the admission throttle
#   is engaged, and refuses to guess when the throttle stamp cannot be read.
#
#   ./test-strand-throttle.sh
#
# WHY THIS EXISTS. sp-lsh22, sp-fmhwi (2026-09-24): the [spira,plan] queue escalated as
# "stranded (starved)" while CHECK7's own log line in the same pass read "throttle active
# (... depth=12 since_land=4m) — task pool held at 0". Ryan's verdict: "nothing was
# stranded; the queue depth was too high to allocate work to aeons" — the detector must
# check the throttle before calling withheld aeons a strand.
#
# FOUR CASES (law-absence-needs-a-positive-control):
#
#   0. POSITIVE CONTROL — THROTTLE_STATE=open, ready beads, no live aeon → classifier
#      DOES produce "starved". Proves the check can fire before we trust its silence.
#
#   1. THROTTLE SHUT — THROTTLE_STATE=shut, same fixture → no "starved" in output;
#      "throttled" IS emitted as an info row naming depth and release-at (never escalate).
#
#   2. THROTTLE UNREADABLE — THROTTLE_STATE=unreadable → neither "starved" nor "throttled"
#      is emitted; a distinct "throttle-unreadable" row is (law-a-control-that-cannot-
#      check-must-refuse: it does not assume open, and it does not assume shut).
#
#   3. throttle_state() itself — sourced from strand.sh against a real stamp file: absent
#      stamp reads "open", a readable stamp reads "shut" with its depth, and a stamp made
#      unreadable (chmod 000) reads "unreadable" rather than silently falling back to
#      either of the other two.
#
# PRE-FIX FAILURE (run against unfixed strand-classify.py / strand.sh):
#
#   FAIL  throttle shut: throttled IS emitted: wanted [throttled] in [starved\t-\tescalate\t...]
#   FAIL  throttle shut: starved NOT emitted: did not want [starved] in [starved\t-\tescalate\t...]
#   FAIL  throttle unreadable: throttle-unreadable IS emitted: wanted [throttle-unreadable] in []
#   FAIL  throttle unreadable: starved NOT emitted: did not want [starved] in [starved\t-\t...]
#   FAIL  throttle_state: unreadable stamp reads 'unreadable': wanted [unreadable] in [open ]
#
# defect: sp-0ua6w
# covers: spira/strand-classify.py spira/strand.sh
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# One ready bead, no live aeons — the fixture that starved fires on.
BEADS='[{"id":"sp-example","title":"test bead","status":"open","labels":["spira","plan"]}]'
READY='[{"id":"sp-example","title":"test bead","status":"open","labels":["spira","plan"]}]'

classify() {
    local throttle_state="${1:-open}" throttle_detail="${2:-}"
    printf '%s' "$BEADS" > "$TMP/beads.json"
    printf '%s' "$READY"  > "$TMP/ready.json"
    BEADS_FILE="$TMP/beads.json" \
    READY_FILE="$TMP/ready.json" \
    HOLDERS="" LIVE=0 GHOST_GRACE=300 \
    SPIRA_ASK_LABEL=needs-operator \
    CAPACITY_PAUSED=0 \
    THROTTLE_STATE="$throttle_state" \
    THROTTLE_DETAIL="$throttle_detail" \
        python3 "$HERE/strand-classify.py"
}

echo "test-strand-throttle.sh"

# ======================================================================================
echo
echo "case 0 — positive control: THROTTLE_STATE=open → classifier DOES produce starved:"
# ======================================================================================
# Without this half, a classifier that never fires looks correct when we test silence in
# cases 1 and 2 (law-absence-needs-a-positive-control).
out="$(classify open)"
want "positive control: starved IS raised" "starved" "$out"

# ======================================================================================
echo
echo "case 1 — throttle shut: THROTTLE_STATE=shut → no starved escalation:"
# ======================================================================================
# The throttle withholding aeons is the queue working as intended, not a strand — the
# same false alarm as a capacity pause. The row must never recommend an escalation.
out="$(classify shut "depth 12 >= release-at 8")"
want   "throttle shut: throttled IS emitted"      "throttled" "$out"
want   "throttle shut: detail names depth/release" "depth 12 >= release-at 8" "$out"
nowant "throttle shut: starved NOT emitted"       "starved"   "$out"
want   "throttle shut: disposition is info, not escalate" $'throttled\t-\tinfo' "$out"

# ======================================================================================
echo
echo "case 2 — throttle unreadable: state cannot be read → refuse, do not guess:"
# ======================================================================================
# law-a-control-that-cannot-check-must-refuse: an unreadable stamp is neither "the
# throttle is open" (would wrongly escalate) nor "the throttle is shut" (would wrongly
# suppress a real strand) — it gets its own row and its own escalation.
out="$(classify unreadable "/run/queue-throttled exists but could not be read")"
want   "throttle unreadable: throttle-unreadable IS emitted" "throttle-unreadable" "$out"
nowant "throttle unreadable: starved NOT emitted"             "starved"            "$out"
nowant "throttle unreadable: throttled NOT emitted"           $'throttled\t-\tinfo' "$out"

# ======================================================================================
echo
echo "case 3 — 'strand.sh throttle-state' against a real stamp file:"
# ======================================================================================
mkdir -p "$TMP/run"
out="$(SPIRA_RUN="$TMP/run" bash "$HERE/strand.sh" throttle-state 2>&1)"
want "throttle_state: no stamp reads open" $'open\t' "$out"

_stamp="$TMP/run/queue-throttled"
printf 'since=2026-09-24T18:01:47Z depth=12 since_land=4m\n' > "$_stamp"
out="$(SPIRA_RUN="$TMP/run" bash "$HERE/strand.sh" throttle-state 2>&1)"
want "throttle_state: readable stamp reads shut" $'shut\tdepth 12' "$out"

chmod 000 "$_stamp"
out="$(SPIRA_RUN="$TMP/run" bash "$HERE/strand.sh" throttle-state 2>&1)"
chmod 644 "$_stamp"
want "throttle_state: unreadable stamp reads unreadable" "unreadable" "$out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
