#!/usr/bin/env bash
#
# test-check2-reclaim.sh — CHECK 2's dead-worker reaper skips IN_PROGRESS beads whose
#   only open dep carries the ask label, and stops skipping once that dep closes.
#
#   ./test-check2-reclaim.sh
#
# WHY THIS EXISTS. sp-mfa4 hit the reclaim loop six times: it was IN_PROGRESS because its
# aeon correctly exited after diagnosing a needs-ryan dep, but CHECK 2's time-based reaper
# cannot distinguish "correctly waiting" from "genuinely dead". It reclaimed the bead,
# summoned a new aeon, which re-derived the same diagnosis and exited — a ~180m-period
# loop. check2_protect_waiting (lib.sh) holds the bead (spira-lc "wait", sp-i2m7y — this was
# the SPIRA_RECLAIM_SKIP_LABEL bd label) while its only open dep carries the ask label, so
# check2_reclaim_stale's own scan skips it. The hold is released when the dep closes,
# letting the reaper reclaim the stale lease on that pass.
#
# THREE CASES, THE ONES check2_protect_waiting's DEP-SHAPE LOGIC NEEDS AND THE
# END-TO-END CHAIN DOES NOT ALREADY COVER. The two cases that only re-asserted "hold
# applied" / "hold released" are dropped here (D7, docs/test-plan/dispatch.md row 21):
# test-reclaim-escalated.sh's chain cases 1 and 2 call this same check2_protect_waiting
# against this same dep shape and already assert the hold on a real spira-lc — carrying
# the assertion on to the classifier besides, which this file's dropped cases did not.
#   1. POSITIVE CONTROL (dead worker, no deps) — no protection, reclaim can fire.
#      Without this, a protect-everything implementation reads as correct.
#   2. NOT PROTECTED (open dep without ask label) — no hold, reaper can fire.
#   3. MIXED DEPS (one ask dep + one non-ask open dep) — no hold: "protect" requires
#      EVERY open dep to carry the ask label, and cases 2/3 are the two ways one doesn't.
#
# A REAL bd ON A FIXTURE DATABASE (law-prefer-the-real-dependency). check2_protect_waiting
# calls bd show for the dependency shape; a stub would drift silently and prove nothing
# about the real dependency-reading path. lc_hold/lc_unhold themselves are stubbed as call
# recorders here rather than run against a real spira-lc server — this file's job is only
# to prove the three dep shapes below never REACH the hold/release calls at all, which a
# recorder catches exactly as well as a real store and without a second dolt server (the
# chain test already exercises the real one).
#
# tier: T2
# defect: sp-rzyl
# covers: sentinel/src/* spira/lib.sh spira/lc.sh UC-dispatch-21
# hermetic-ok: uses a fixture database, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check2-reclaim
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up check2reclaim || { echo "test-check2-reclaim: could not build fixture database"; exit 1; }

# Stub what sentinel.sh defines but lib.sh needs.
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
acted=0; progressed=0
act()      { acted=$((acted+1)); }
progress() { progressed=$((progressed+1)); act "$@"; }
log()      { : ; }   # suppress log noise in test output
# shellcheck disable=SC1090
. "$HERE/lib.sh"
# shellcheck disable=SC1090
. "$HERE/lc.sh"

# lc_hold/lc_unhold as call recorders, not the real spira-lc client — see the file header.
HOLD_CALLS="$TMP/hold-calls"; : > "$HOLD_CALLS"
lc_hold()   { printf 'hold %s %s\n'   "$1" "$2" >> "$HOLD_CALLS"; }
lc_unhold() { printf 'unhold %s %s\n' "$1" "$2" >> "$HOLD_CALLS"; }

echo "test-check2-reclaim.sh"

# The ask label used for these tests (fixture configuration).
ASK="${SPIRA_ASK_LABEL:-needs-operator}"

# ======================================================================================
echo
echo "case 1 — positive control: dead worker (no deps) is NOT given the wait hold:"
# ======================================================================================
# An aeon died for an unrelated reason; its bead has no deps. check2_protect_waiting must
# NOT mark it as protected — that would block the reaper from doing its job.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-dead1","title":"dead worker","status":"in_progress","issue_type":"task","labels":["plan","repo:spira","spira"],"assignee":"aeon-dead"}
JSONL
acted=0; : > "$HOLD_CALLS"
check2_protect_waiting
is "dead worker: no lc_hold/lc_unhold call" "" "$(cat "$HOLD_CALLS")"
is "dead worker: no act recorded" "0" "$acted"

# ======================================================================================
echo
echo "case 2 — not protected: open dep without ask label → no wait hold:"
# ======================================================================================
# The bead has an open dep that is NOT a needs-ryan bead (a normal work dep). The aeon
# did not correctly-pause on a ryan dep; it may be a genuine dead worker. No protection.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-other1","title":"prerequisite","status":"open","issue_type":"task","labels":["plan","repo:spira","spira"],"assignee":""}
{"id":"sp-work3","title":"work with non-ryan dep","status":"in_progress","issue_type":"task","labels":["plan","repo:spira","spira"],"assignee":"aeon-y","dependencies":[{"depends_on_id":"sp-other1","type":"blocks"}]}
JSONL
acted=0; : > "$HOLD_CALLS"
check2_protect_waiting
is "non-ryan dep: no lc_hold/lc_unhold call" "" "$(cat "$HOLD_CALLS")"
is "non-ryan dep: no act recorded" "0" "$acted"

# ======================================================================================
echo
echo "case 3 — mixed deps: one ask dep + one non-ask open dep → not protected:"
# ======================================================================================
# The bead has BOTH a ryan dep AND a regular dep open. The condition for protection is
# "all open deps carry the ask label". One non-ask dep disqualifies it.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-ask3","title":"ryan decision","status":"open","issue_type":"decision","labels":["$ASK","plan","spira"],"assignee":""}
{"id":"sp-other2","title":"other prereq","status":"open","issue_type":"task","labels":["plan","repo:spira","spira"],"assignee":""}
{"id":"sp-work4","title":"work with mixed deps","status":"in_progress","issue_type":"task","labels":["plan","repo:spira","spira"],"assignee":"aeon-z","dependencies":[{"depends_on_id":"sp-ask3","type":"blocks"},{"depends_on_id":"sp-other2","type":"blocks"}]}
JSONL
acted=0; : > "$HOLD_CALLS"
check2_protect_waiting
is "mixed deps: no lc_hold/lc_unhold call" "" "$(cat "$HOLD_CALLS")"
is "mixed deps: no act recorded" "0" "$acted"

tl_summary
