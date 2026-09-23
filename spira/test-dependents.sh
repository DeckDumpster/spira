#!/usr/bin/env bash
#
# test-dependents.sh — a ready bead whose closed blocker is in the queue pipeline
#   (CERTIFIED/BATCHED landstate) is held by SPIRA_QUEUE_WAIT_LABEL until the blocker
#   reaches LANDED; once it does, the bead is summonable again.
#
# THE PROBLEM. bd considers a dep resolved when the blocker is closed. In queue mode,
# CLOSED ≠ LANDED — the work is in the batch pipeline but has not yet been pushed to base.
# Summoning the dependent now would let it build on work that may still be revised or
# rejected by the batch gate.
#
# THE FIX. mark_queue_waiters (lib.sh) scans landstate files. If a closed bead's landstate
# is CERTIFIED or BATCHED, the bead is an active queue blocker. Any ready bead whose
# dependency graph has that blocker on a "blocks" edge gets SPIRA_QUEUE_WAIT_LABEL applied.
# fayth_exclude (lib.sh) adds the label to every fayth's exclusion list, so fayth_ready
# returns 0 for the dependent. When the blocker's landstate reaches LANDED, mark_queue_waiters
# removes the label and the dependent becomes summonable.
#
# WHAT THIS SUITE CHECKS (17 assertions).
#   1. POSITIVE CONTROL (label applied): mark_queue_waiters labels the dependent when the
#      blocker has a CERTIFIED landstate. Without this, a sweep that labels nothing passes.
#   2. State check: the queue-wait label is actually on the dependent in the db.
#   3. Summon exclusion: fayth_ready returns 0 after labeling (bead excluded).
#   4. Second pass idempotent: second mark_queue_waiters call does not error.
#   5. LANDED clears label: updating landstate to LANDED causes mark_queue_waiters to remove it.
#   6. State check: the queue-wait label is gone from the dependent.
#   7. Summon restored: fayth_ready returns >0 after the blocker lands.
#   8. No landstate = not labeled: a closed bead with no landstate file is not a queue blocker.
#   9. GATED state (push-mode) does not trigger: push-mode intermediates are not queue blockers.
#  10. fayth_exclude includes SPIRA_QUEUE_WAIT_LABEL in the exclude arg to ready_count.
#  11. CERTIFIED tip=none: a commit-less closed bead (design/diagnosis) is not a queue blocker.
#  12. Two-blocker apply: dep gets label when real blocker is CERTIFIED, commit-less is ignored.
#  13. Two-blocker single-pass clear: when real blocker lands, label cleared in one pass
#      (not cleared-then-reapplied for the commit-less blocker).
#  14. Summon restored after two-blocker clear.
#  15. POSITIVE CONTROL: close_landed_queue_waiters closes a bead with LANDED landstate and
#      the wait label. Without this, the function doing nothing still passes 16-17.
#  16. State check: the closed bead no longer carries the wait label.
#  17. CERTIFIED waiter stays open: a bead with CERTIFIED (not LANDED) is not closed.
#
# defect: sp-v890d sp-ya5nk sp-rvoun
# covers: spira/lib.sh spira/sentinel.sh spira/conf.sh spira/strand.sh
# hermetic-ok: uses a fixture database; mark_queue_waiters tested with real bd;
#              assertion 10 uses stub ready_count (no db call needed for structural check)
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()    { [ "$2" = "$3" ]        && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
has()   { [[ "$3" == *"$2"* ]]   && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lacks() { [[ "$3" != *"$2"* ]]   && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
gt0()   { [ "${2:-0}" -gt 0 ]    && ok "$1" || bad "$1" "wanted >0 got [$2]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-dependents
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up dependents || { echo "test-dependents: could not build fixture database"; exit 1; }

RUN="$TMP/run"
LANDSTATE="$RUN/landstate"
mkdir -p "$LANDSTATE"

export SPIRA_RUN="$RUN"
export SPIRA_HOME="$HERE"
export SPIRA_CONF="$TMP/no.conf"   # no host config leaking into the suite
log() { :; }                        # suppress log noise
# shellcheck disable=SC1090
. "$HERE/lib.sh"

B() { bd -C "$SPIRA_DB" "$@"; }
labels_of() {
    B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))' 2>/dev/null
}

WAIT="${SPIRA_QUEUE_WAIT_LABEL:-spira-queue-waiting}"

# Seed: sp-blocker is closed (bd sees it resolved); sp-dependent is open and unblocked
# by bd's own rules — but its blocker is in the queue pipeline, not yet on base.
seed() {
    testdb_reset
    testdb_seed <<JSONL
{"id":"sp-blocker","title":"blocker","status":"closed","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z"}
{"id":"sp-dependent","title":"dependent","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-dependent","depends_on_id":"sp-blocker","type":"blocks"}]}
JSONL
    # Reset landstate for a clean fixture each case.
    rm -f "$LANDSTATE/sp-blocker"
}

echo "test-dependents.sh"

# =====================================================================================
echo
echo "assertions 1-3 — CERTIFIED blocker: label applied, bead excluded from summon:"
# =====================================================================================
# POSITIVE CONTROL. Without planting a CERTIFIED landstate and requiring mark_queue_waiters
# to label the dependent, a sweep that labels nothing passes every silence below for free.
seed
printf 'CERTIFIED abc123 %s\n' "$(date +%s)" > "$LANDSTATE/sp-blocker"

mark_queue_waiters 2>/dev/null
has   "1: CERTIFIED blocker — queue-wait label applied to dependent" \
      "$WAIT" "$(labels_of sp-dependent)"

has   "2: label is on the bead in the db" \
      "$WAIT" "$(B label list sp-dependent 2>/dev/null)"

# fayth_ready uses fayth_exclude, which now includes SPIRA_QUEUE_WAIT_LABEL. The labeled
# bead is passed to ready_count with --exclude-label containing the wait label, so count=0.
is    "3: fayth_ready builder returns 0 (bead excluded)" \
      "0" "$(fayth_ready builder 2>/dev/null)"

# =====================================================================================
echo
echo "assertion 4 — second pass is idempotent:"
# =====================================================================================
mark_queue_waiters 2>/dev/null
has   "4: second mark_queue_waiters call leaves label in place" \
      "$WAIT" "$(labels_of sp-dependent)"

# =====================================================================================
echo
echo "assertions 5-7 — LANDED blocker: label removed, bead becomes summonable:"
# =====================================================================================
# Update landstate to LANDED. The bead is no longer an active queue blocker.
printf 'LANDED abc123 %s spira\n' "$(date +%s)" > "$LANDSTATE/sp-blocker"

mark_queue_waiters 2>/dev/null
lacks "5: LANDED blocker — queue-wait label removed from dependent" \
      "$WAIT" "$(labels_of sp-dependent)"

lacks "6: label is gone from the db" \
      "$WAIT" "$(B label list sp-dependent 2>/dev/null)"

gt0   "7: fayth_ready builder returns >0 (bead is now summonable)" \
      "$(fayth_ready builder 2>/dev/null)"

# =====================================================================================
echo
echo "assertion 8 — no landstate file: closed bead is not treated as queue blocker:"
# =====================================================================================
# A closed bead with no landstate entry is push-mode or was landed before queue mode was
# introduced. mark_queue_waiters must not label the dependent for it.
seed   # fresh: sp-blocker closed, no landstate file
mark_queue_waiters 2>/dev/null
lacks "8: no landstate file — dependent is not labeled" \
      "$WAIT" "$(labels_of sp-dependent)"

# =====================================================================================
echo
echo "assertion 9 — GATED state (push-mode intermediate): does not trigger:"
# =====================================================================================
# GATED is written by the push arm of landing.sh; it is not a queue-pipeline state.
# The dependent must not get the wait label.
seed
printf 'GATED abc123 %s gate-result:PASS\n' "$(date +%s)" > "$LANDSTATE/sp-blocker"

mark_queue_waiters 2>/dev/null
lacks "9: GATED landstate — push-mode state does not trigger queue-wait label" \
      "$WAIT" "$(labels_of sp-dependent)"

# =====================================================================================
echo
echo "assertion 10 — fayth_exclude passes SPIRA_QUEUE_WAIT_LABEL in exclude arg:"
# =====================================================================================
# A structural check: the label must reach ready_count's second arg via fayth_exclude,
# not just be set in conf.sh. Uses stub ready_count (no db call needed).
EXCL_FILE="$TMP/observed-excl"
MOCK_READY=0
ready_count() {
    printf '%s' "$2" > "$EXCL_FILE"
    printf '%d' "$MOCK_READY"
}
aeon_count() { printf '0'; }

fayth_ready builder >/dev/null 2>&1 || true
observed_excl="$(cat "$EXCL_FILE" 2>/dev/null)"
has "10: fayth_exclude passes SPIRA_QUEUE_WAIT_LABEL in exclude arg to ready_count" \
    "$WAIT" "$observed_excl"

# =====================================================================================
echo
echo "assertion 11 — CERTIFIED with tip=none: commit-less blocker is not a queue blocker:"
# =====================================================================================
# POSITIVE CONTROL. A CERTIFIED landstate with tip="none" means no commit was recorded
# (design, diagnosis, superseded bead). It will never reach LANDED by the queue path
# and must not hold its dependents in queue-wait.
seed
printf 'CERTIFIED none %s\n' "$(date +%s)" > "$LANDSTATE/sp-blocker"

mark_queue_waiters 2>/dev/null
lacks "11: CERTIFIED tip=none — commit-less blocker does not label dependent" \
      "$WAIT" "$(labels_of sp-dependent)"

# =====================================================================================
echo
echo "assertions 12-14 — two-blocker scenario: one real, one commit-less:"
# =====================================================================================
# A dependent has two blockers: sp-blocker-real (CERTIFIED, real commit) and
# sp-blocker-design (CERTIFIED, tip=none, a design bead). The design bead must not
# keep the dependent labeled after sp-blocker-real lands; the label must be cleared
# in a single pass (not cleared-then-reapplied for the design bead).
testdb_reset
testdb_seed <<JSONL
{"id":"sp-blocker-real","title":"real blocker","status":"closed","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z"}
{"id":"sp-blocker-design","title":"design blocker","status":"closed","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z"}
{"id":"sp-two","title":"two-blocker dependent","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-two","depends_on_id":"sp-blocker-real","type":"blocks"},{"issue_id":"sp-two","depends_on_id":"sp-blocker-design","type":"blocks"}]}
JSONL
rm -f "$LANDSTATE/sp-blocker-real" "$LANDSTATE/sp-blocker-design"
printf 'CERTIFIED abc123 %s\n' "$(date +%s)" > "$LANDSTATE/sp-blocker-real"
printf 'CERTIFIED none   %s\n' "$(date +%s)" > "$LANDSTATE/sp-blocker-design"

mark_queue_waiters 2>/dev/null
has   "12: real blocker CERTIFIED — queue-wait label applied" \
      "$WAIT" "$(labels_of sp-two)"

# Now sp-blocker-real reaches LANDED. sp-blocker-design is still CERTIFIED/none.
# The label must be cleared in one pass — not cleared-for-real and reapplied-for-design.
LOGGED="$TMP/mqw.log"
log() { printf '%s\n' "$*" >> "$LOGGED"; }
printf 'LANDED abc123 %s spira\n' "$(date +%s)" > "$LANDSTATE/sp-blocker-real"
rm -f "$LOGGED"

mark_queue_waiters 2>/dev/null
log_out="$(cat "$LOGGED" 2>/dev/null)"
log() { :; }   # restore quiet log

lacks "13: single-pass clear — label not reapplied after clear in same pass" \
      "queue-wait applied" "$log_out"
lacks "14: sp-two no longer labeled after real blocker lands" \
      "$WAIT" "$(labels_of sp-two)"

# =====================================================================================
echo
echo "assertions 15-17 — close_landed_queue_waiters: LANDED bead with wait label closed:"
# =====================================================================================
# A bead that already has landstate=LANDED but still carries SPIRA_QUEUE_WAIT_LABEL
# was never visited by the landing pass (no branch to land), so it stayed open.
# close_landed_queue_waiters must close it and clear the label.
#
# POSITIVE CONTROL (15): plant the label + LANDED landstate; the function must close it.
# STATE CHECK (16): the bead is closed in the db.
# CERTIFIED DOES NOT CLOSE (17): a bead with CERTIFIED (not LANDED) must be left open.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-landed-waiter","title":"already landed","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","$WAIT"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-certified-waiter","title":"still in queue","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","$WAIT"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
printf 'LANDED abc123 %s spira\n' "$(date +%s)" > "$LANDSTATE/sp-landed-waiter"
printf 'CERTIFIED abc456 %s\n' "$(date +%s)" > "$LANDSTATE/sp-certified-waiter"

close_landed_queue_waiters 2>/dev/null

is "15: LANDED waiter — close_landed_queue_waiters closes it" \
   "closed" "$(B show sp-landed-waiter --json 2>/dev/null | python3 -c \
       'import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("status",""))' 2>/dev/null)"
lacks "16: LANDED waiter — wait label cleared" \
   "$WAIT" "$(labels_of sp-landed-waiter)"
is "17: CERTIFIED waiter — still open (not closed by closeout pass)" \
   "open" "$(B show sp-certified-waiter --json 2>/dev/null | python3 -c \
       'import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("status",""))' 2>/dev/null)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
