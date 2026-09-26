#!/usr/bin/env bash
#
# test-check4-unit.sh — check4_decide, the CHECK 4 decision extracted to a pure function.
#
#   ./test-check4-unit.sh
#
# check4_decide takes attempts, requeues, reclaims, labels and an asked-stamp (dedup state
# the caller already read) and returns which of poison/clear/ask/requeue-mail/reclaim-mail
# apply — no bd call, no file read. This table replaces what used to require a full sentinel
# pass on a server-mode Dolt store: test-poison.sh (304s), test-poison-edge.sh (228s),
# test-poison-ask.sh (179s), test-check4-events.sh's criterion 3 (127s), test-check4-batch.sh's
# criterion 3 (103s), test-requeue-cap.sh and test-requeue-cap-accept.sh (448s, deleted
# 2026-09-25 for flipping — law-a-test-that-flips-is-deleted — leaving the requeue/reclaim
# cap and cross-partition behaviour they covered untested until now).
#
# D1-D4 (duplicate clusters, docs/test-plan/aeon-execution.md section 4): the "3 attempts ->
# poison" fixture six files each rebuilt, the requeue-cap dedup two files split "purely for
# wall time", the stale-clear check two files ran, and the ask-title wording two files
# asserted verbatim — one table, one row each.
#
# G11 (reclaim cap): SPIRA_RECLAIM_AT was set by test-requeue-cap-accept.sh but nothing
# asserted it fired, and the sentinel's own per-bead loop hardcoded reclaims=0 — the cap
# arithmetic was untested and the wiring that would have carried real data was dead code.
# Both halves are closed here: the arithmetic below, and the wiring in
# spira/sentinel.sh + check4_bulk_data's fourth column.
#
# G14 (CHECK 4 performance, sp-f1m7f's motive: "no per-bead queries"): stub
# attempts_of/reopens_of/reclaims_of to fail loudly if called, then run every row in this
# table through check4_decide. The old per-bead-query loop is gone; this is the proof it
# cannot silently come back through the seam that replaced it.
#
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control): a decision that fires sits
# beside the same inputs one field short of firing.
#
# No database, no network, under a second.
#
# tier: T1
# defect: sp-mqnf sp-njwb sp-fx1p sp-pi3ez sp-f1m7f sp-lzt sp-wiyr2
# covers: spira/lib.sh spira/sentinel.sh spira/attempts.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# c4d <attempts> <requeues> <reclaims> <labels> <asked-stamp> [poison_at] [requeue_at] [reclaim_at]
# -> check4_decide's real output, sourced fresh each call so no case can leak state into
# the next through an exported POISON_AT/REQUEUE_AT/RECLAIM_AT.
c4d() {
    local n="$1" rq="$2" rc="$3" labels="$4" stamp="$5"
    local p_at="${6:-3}" r_at="${7:-5}" c_at="${8:-5}"
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        POISON_AT="$p_at" REQUEUE_AT="$r_at" RECLAIM_AT="$c_at" \
        bash -c '. "$1"/lib.sh; check4_decide "$2" "$3" "$4" "$5" "$6"' \
        _ "$HERE" "$n" "$rq" "$rc" "$labels" "$stamp" 2>/dev/null
}

echo "poison / ask — threshold, zero-attempt guard, POISON_AT=0 edge (D1, UC-21):"

is "below threshold: no decision"                    "none"        "$(c4d 2 0 0 "spira,plan" 0:0:0)"
is "AT threshold, none asked: poison and ask"         "poison ask"  "$(c4d 3 0 0 "spira,plan" 0:0:0)"
is "above threshold, none asked: poison and ask too"  "poison ask"  "$(c4d 5 0 0 "spira,plan" 0:0:0)"
is "already labeled, not yet asked at this count: ask only" "ask"   "$(c4d 3 0 0 "spira,plan,spira-poison" 0:0:0)"
is "already labeled and already asked at this count: none"  "none"  "$(c4d 3 0 0 "spira,plan,spira-poison" 0:0:1)"
is "zero attempts at POISON_AT=0: no decision (nothing failed)" "none" \
   "$(c4d 0 0 0 "spira,plan" 0:0:0 0)"
is "CONTROL: one attempt at POISON_AT=0 still poisons"      "poison ask" \
   "$(c4d 1 0 0 "spira,plan" 0:0:0 0)"

echo
echo "a recorded lift (sp-wiyr2): attempts.sh deadlocked takes the label off but not the"
echo "count, so the fourth stamp field is what stops the very next pass poisoning it back:"

is "lifted at this count, already asked: no re-poison"        "none" \
   "$(c4d 3 0 0 "spira,plan" 0:0:1:1)"
is "CONTROL: same count, no lift recorded: poison fires"       "poison" \
   "$(c4d 3 0 0 "spira,plan" 0:0:1:0)"
is "a missing fourth field (pre-fix caller) reads as not lifted: poison fires" "poison" \
   "$(c4d 3 0 0 "spira,plan" 0:0:1)"

# poison_lifted ITSELF is what keeps a genuinely new failure from being swallowed by an old
# lift: check4_decide only sees the boolean the caller already resolved, so the "still
# poisons past the lifted count" property has to be proven against the function that
# resolves it, not against check4_decide's table.
export SPIRA_RUN="$TMP/lifted-run"
lifted() {
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_RUN="$SPIRA_RUN" \
        bash -c '. "$1"/lib.sh; poison_lifted "$2" "$3" && echo yes || echo no' \
        _ "$HERE" "$1" "$2" 2>/dev/null
}
mark() {
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_RUN="$SPIRA_RUN" \
        bash -c '. "$1"/lib.sh; poison_lifted_mark "$2" "$3"' \
        _ "$HERE" "$1" "$2" 2>/dev/null
}
is "before any lift is recorded: not lifted"           "no"  "$(lifted sp-x 3)"
mark sp-x 3
is "lifted at exactly the count it was recorded at"    "yes" "$(lifted sp-x 3)"
is "a NEW failure past the lifted count is not covered" "no"  "$(lifted sp-x 4)"
mark sp-x 5
is "a later lift at a higher count re-covers the bead" "yes" "$(lifted sp-x 5)"

echo
echo "ask dedup is keyed on (bead, count), not the label (D4, UC-22):"

is "a higher count re-asks (the caller resolves per-count dedup before calling)" "ask" \
   "$(c4d 4 0 0 "spira,plan,spira-poison" 0:0:0)"
is "clearing the label re-poisons at the SAME count but does not re-arm ITS ask" "poison" \
   "$(c4d 3 0 0 "spira,plan" 0:0:1)"

echo
echo "stale poison clear (D3, UC-21):"

is "labeled bead below threshold: clear"              "clear" "$(c4d 1 0 0 "spira,plan,spira-poison" 1:1:1)"
is "labeled bead AT threshold: kept, not cleared"      "none"  "$(c4d 3 0 0 "spira,plan,spira-poison" 1:1:1)"
is "unlabeled bead below threshold: nothing to clear"  "none"  "$(c4d 1 0 0 "spira,plan" 1:1:1)"

echo
echo "requeue cap: threshold, dedup, delivers:action exemption (D2, UC-23):"

is "below cap: no decision"                            "none"          "$(c4d 0 4 0 "spira,plan" 0:0:0)"
is "AT cap, not yet asked: requeue-mail"                "requeue-mail"  "$(c4d 0 5 0 "spira,plan" 0:0:0)"
is "cap+1, already asked: no re-mail (dedup is per-bead, not per-count)" "none" \
   "$(c4d 0 6 0 "spira,plan" 1:0:0)"
is "cap+10, already asked: still no re-mail"            "none"          "$(c4d 0 15 0 "spira,plan" 1:0:0)"
is "delivers:action is exempt even over the cap"        "none"          "$(c4d 0 9 0 "spira,plan,delivers:action" 0:0:0)"

echo
echo "reclaim cap: threshold and dedup (G11 — untested before this seam):"

is "below cap: no decision"                             "none"          "$(c4d 0 0 4 "spira,plan" 0:0:0)"
is "AT cap, not yet asked: reclaim-mail"                 "reclaim-mail"  "$(c4d 0 0 5 "spira,plan" 0:0:0)"
is "already asked: no re-mail"                           "none"          "$(c4d 0 0 5 "spira,plan" 0:1:0)"
is "reclaim cap has no delivers:action exemption (the box killed the worker, not the queue)" \
   "reclaim-mail" "$(c4d 0 0 5 "spira,plan,delivers:action" 0:0:0)"

echo
echo "the three concerns are independent — none is exempted by another firing (fixes a"
echo "'continue past the rest of the checks once dedup already fired' quirk in the old inline code):"

is "over requeue cap (already asked) AND at poison threshold: poison still fires" \
   "poison ask" "$(c4d 3 6 0 "spira,plan" 1:0:0)"
is "over reclaim cap (already asked) AND at poison threshold: poison still fires" \
   "poison ask" "$(c4d 3 0 6 "spira,plan" 0:1:0)"
is "all three at once, nothing yet asked: all four tokens" \
   "requeue-mail reclaim-mail poison ask" "$(c4d 3 5 5 "spira,plan" 0:0:0)"

echo
echo "G14 — check4_decide makes no bd call: attempts_of/reopens_of/reclaims_of stubbed to"
echo "fail loudly if invoked, then the whole table above runs again through the stub:"

MARKER="$TMP/per-bead-query-called"
c4d_guarded() {
    local n="$1" rq="$2" rc="$3" labels="$4" stamp="$5"
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        POISON_AT=3 REQUEUE_AT=5 RECLAIM_AT=5 MARKER="$MARKER" \
        bash -c '. "$1"/lib.sh
attempts_of() { echo "$MARKER" >> "$MARKER"; return 1; }
reopens_of()  { echo "$MARKER" >> "$MARKER"; return 1; }
reclaims_of() { echo "$MARKER" >> "$MARKER"; return 1; }
check4_decide "$2" "$3" "$4" "$5" "$6"' \
        _ "$HERE" "$n" "$rq" "$rc" "$labels" "$stamp" 2>/dev/null
}
rm -f "$MARKER"
c4d_guarded 3 0 0 "spira,plan" 0:0:0 >/dev/null
c4d_guarded 3 6 0 "spira,plan,delivers:action" 1:0:0 >/dev/null
c4d_guarded 0 0 5 "spira,plan" 0:0:0 >/dev/null
c4d_guarded 1 0 0 "spira,plan,spira-poison" 1:1:1 >/dev/null
is "no per-bead query function was called across the table" "" \
   "$([ -f "$MARKER" ] && cat "$MARKER" || printf '')"

tl_summary
