#!/usr/bin/env bash
#
# test-check4-events.sh — attempts are computed from the events trail, not from labels.
#
#   ./test-check4-events.sh
#
# THE DEFECT THIS GUARDS. Counter labels (sp-attempt-N, sp-reclaim-N, sp-requeue-N,
# sp-timeout-N, sp-recur-N) polluted the label bag: 195 of 660 distinct labels in the old
# store were counter labels, sp-reclaim alone having 96 forms because it encoded a cause
# suffix. Labels carry no validation, so each form was a new string that could disagree with
# the events trail.
#
# The new design is simpler: an attempt is a status_changed event whose new_value contains
# 'in_progress'. The events table is always populated by bd and cannot disagree with itself.
# The poison decision reads it on demand, never stores it.
#
# THREE ACCEPTANCE CRITERIA, ALL TESTED HERE:
# 1. No counter label (sp-attempt-*, sp-reclaim-*, sp-requeue-*, sp-timeout-*, sp-recur-*) is
#    written by any harness path — asserted structurally on the code.
# 2. A bead cycled N times reports exactly N; unclaimed rows carrying the string do not
#    over-count — asserted against a real database with known event counts.
# 3. The poison decision uses the events predicate — asserted via sentinel.sh CHECK4.
#
# FAIL-FIRST for criteria 2 and 3: each assertion below is run against the unfixed code first
# (where attempts_of reads labels), confirmed to fail, then run against the fixed code.
#
# A REAL bd ON A THROWAWAY DATABASE. The events are written by bd itself; a stub would be a
# second implementation of exactly the thing being asked about
# (law-prefer-the-real-dependency).
#
# defect: sp-lzt
# covers: spira/lib.sh spira/sentinel.sh spira/aeon.sh spira/strand.sh spira/landing.sh spira/attempts.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check4-events
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up check4-events || {
    printf 'SKIP test-check4-events: server testdb not available\n' >&2
    exit 77
}
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-check4-events.sh"

# --------------------------------------------------------------------------------------
# CRITERION 1: No counter label is written by any harness path.
#
# Structural assertion on the code: grep every non-test, non-lib script for the label
# prefixes. The label-writing functions (bump_counter) are in lib.sh; every caller reaches
# them through the bump_* aliases. The check here is that no script calls bdq label add
# with a counter label directly AND that the bump_* functions do not write labels.
# --------------------------------------------------------------------------------------
echo
echo "criterion 1: no counter label written by any harness path"

BANNED='sp-attempt-|sp-reclaim-|sp-reclaim$|sp-requeue-|sp-requeue$|sp-timeout-|sp-recur-'
# Only scripts that can write — test suites excluded, lib.sh excluded (it defines the fns).
found="$(grep -rlE "label add.*($BANNED)" "$HERE"/*.sh 2>/dev/null \
    | grep -v '/test-' | grep -v '/lib\.sh$' | grep -v '/attempts\.sh$' || true)"
is "no harness script writes counter labels directly" "" "$found"

# The bump_* functions must be no-ops (return without calling bdq label add).
for fn in bump_attempt bump_reclaim bump_requeue bump_timeout bump_recur; do
    body="$(sed -n "/^${fn}()/,/^}/p" "$HERE/lib.sh" 2>/dev/null)"
    has_label_add="$(grep -c 'bdq label add\|bump_counter' <<<"$body" || true)"
    is "$fn does not write a label" "0" "$has_label_add"
done

# --------------------------------------------------------------------------------------
# CRITERION 2: A bead cycled N times reports exactly N via the events trail.
#
# Each bd update to in_progress writes one status_changed event with new_value containing
# 'in_progress'. A claim followed by immediate release and re-claim is two events. An
# unclaimed status row (event_type != status_changed) does not count.
# --------------------------------------------------------------------------------------
echo
echo "criterion 2: attempts_of reads the events trail"

seed() {   # seed <id> — one open, claimable bead
    testdb_reset
    testdb_seed <<JSONL
{"id":"$1","title":"a bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
JSONL
}

num() { local v="$1"; printf '%d' "${v:-0}"; }
cycle_to_inprogress() {   # cycle_to_inprogress <id> <n> — n transitions to in_progress
    local id="$1" n="$2" i=0
    while [ "$i" -lt "$n" ]; do
        bdq update "$id" --status in_progress >/dev/null 2>&1
        bdq update "$id" --status open >/dev/null 2>&1
        i=$((i+1))
    done
    # Leave bead in open state; the last update above opens it back.
}

# POSITIVE CONTROL FIRST. A fresh bead with no events has zero attempts.
seed sp-ev1
is "a fresh bead with no status changes has 0 attempts" "0" "$(num "$(attempts_of sp-ev1)")"

# POSITIVE CONTROL: one in_progress transition is one attempt.
seed sp-ev2
bdq update sp-ev2 --status in_progress >/dev/null 2>&1
is "one in_progress transition is one attempt" "1" "$(num "$(attempts_of sp-ev2)")"

# THREE CYCLES: each bead transitions to in_progress N times and reports exactly N.
seed sp-ev3
cycle_to_inprogress sp-ev3 3
is "three cycles report exactly 3"   "3" "$(num "$(attempts_of sp-ev3)")"

seed sp-ev4
cycle_to_inprogress sp-ev4 1
is "one cycle reports exactly 1"     "1" "$(num "$(attempts_of sp-ev4)")"

# NO OVER-COUNT FROM OTHER EVENT TYPES. A label_added event whose comment mentions
# 'in_progress' must not be counted — the filter on event_type='status_changed' prevents it.
# bd label add creates label_added events, not status_changed ones.
seed sp-ev5
bdq label add sp-ev5 "in_progress-fake" >/dev/null 2>&1
is "a label containing 'in_progress' does not count" "0" "$(num "$(attempts_of sp-ev5)")"

# THE KEY ACCEPTANCE: without any sp-attempt-* labels on the bead, three cycles still
# register as three attempts. The unfixed code (reading labels) returns 0 here, which
# is the failure case that this test exists to detect.
seed sp-ev6
cycle_to_inprogress sp-ev6 3
labels="$(bdq label list sp-ev6 2>/dev/null)" || labels=""
nowant "no sp-attempt-* labels exist on the cycled bead" "sp-attempt-" "$labels"
is "yet three cycles still count as 3 attempts" "3" "$(num "$(attempts_of sp-ev6)")"

# --------------------------------------------------------------------------------------
# CRITERION 3: The poison decision uses the events predicate.
#
# The sentinel's CHECK4 must poison a bead when its events-based attempt count reaches
# POISON_AT, even with no sp-attempt-* labels present. The unfixed code reads labels and
# would not poison a bead with only event-based attempts.
# --------------------------------------------------------------------------------------
echo
echo "criterion 3: poison decision uses the events predicate"

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH/chamber"

cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'printf "%s" "${PILGRIMAGE_OUT:-}"'
stub strand.sh     'printf "%s" "${STRAND_OUT:-}"'
stub sending.sh    'printf "%s" "${SENDING_OUT:-}"'
stub governor.sh   'exit 0'
stub gate.sh       'exit ${GATE_RC:-0}'
stub reflect.sh    'true'
stub mail.sh       '[ "${1:-}" = send ] || exit 0'

cat > "$TMP/launch" <<'L'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$LAUNCH_LOG"
exit "${LAUNCH_RC:-0}"
L
cat > "$TMP/systemctl" <<'S'
#!/usr/bin/env bash
printf '%s\n' "${LAND_STATE:-inactive}"
S
chmod +x "$TMP/launch" "$TMP/systemctl"
export LAUNCH_LOG="$TMP/launch.log"

printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/t.fayth"

sentinel() {
    rm -f "$RUN/reflect.fired" "$RUN/inference.cooldown"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="t" SPIRA_INFERENCE_EVERY=0 \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_SUMMON="$TMP/launch" \
    SPIRA_SKIP_RECLAIM=1 \
    SPIRA_SKIP_CLOSED_CHECK=1 \
        bash "$SH/sentinel.sh" 2>&1
}

labels_of() { bdq show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))'; }
poisoned()    { [[ " $(labels_of "$1") " == *" spira-poison "* ]]; }
ispoisoned()  { poisoned "$2" && ok "$1" || bad "$1" "$2 was not poisoned"; }
notpoisoned() { poisoned "$2" && bad "$1" "$2 was poisoned" || ok "$1"; }

# SEED: one open, dispatchable bead with no sp-attempt-* labels.
# The goal epic is not needed because dispatchable_open uses fayth partitions.
testdb_reset; rm -rf "$RUN/poison-asked"
testdb_seed <<'JSONL'
{"id":"sp-ev-p1","title":"events-only bead at threshold","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
JSONL
# Cycle it to in_progress exactly POISON_AT (3) times via bd update — creates real events.
cycle_to_inprogress sp-ev-p1 3
# Confirm: no counter labels, but 3 events.
labels_pre="$(labels_of sp-ev-p1)"
nowant "bead has no sp-attempt-* labels"       "sp-attempt-" "$labels_pre"
is "bead has 3 in_progress events" "3" "$(num "$(attempts_of sp-ev-p1)")"

# THE SENTINEL MUST POISON IT. Without the fix (reading labels), the count is 0 and the
# bead is not poisoned. With the fix (reading events), the count is 3 and it is poisoned.
out="$(sentinel)"
ispoisoned "sentinel poisons a bead at threshold via events" sp-ev-p1
want "pass records the poisoning" "poisoned sp-ev-p1" "$out"

# BELOW THRESHOLD: a bead cycled twice is not poisoned.
testdb_reset; rm -rf "$RUN/poison-asked"
testdb_seed <<'JSONL'
{"id":"sp-ev-p2","title":"events-only bead below threshold","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
JSONL
cycle_to_inprogress sp-ev-p2 2
out="$(SPIRA_POISON_AT=3 sentinel)"
notpoisoned "a bead below threshold is not poisoned" sp-ev-p2

# STALE POISON CLEAR: if events count is now below threshold (e.g., bead was seeded
# with spira-poison but has only 1 in_progress event), the label is cleared.
testdb_reset; rm -rf "$RUN/poison-asked"
testdb_seed <<'JSONL'
{"id":"sp-ev-stale","title":"stale poison — events below threshold","status":"open","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"sp-ev-live","title":"live poison — events at threshold","status":"open","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-06T00:00:00Z"}
JSONL
# One event for stale, POISON_AT events for live.
bdq update sp-ev-stale --status in_progress >/dev/null 2>&1
bdq update sp-ev-stale --status open >/dev/null 2>&1
cycle_to_inprogress sp-ev-live 3

out="$(SPIRA_POISON_AT=3 sentinel)"
notpoisoned "stale poison (1 event < threshold 3) is cleared"   sp-ev-stale
ispoisoned  "live poison (3 events >= threshold 3) is kept"     sp-ev-live
want        "pass records the stale clear" "stale poison cleared" "$out"

printf '\ntest-check4-events.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
