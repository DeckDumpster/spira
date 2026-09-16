#!/usr/bin/env bash
#
# test-answers-silent-seed.sh — two defects in the answer-delivery leg (sp-6clb)
#
# DEFECT 1. In loop mode, emit's exit status is discarded: a Python crash
# loops silently and Restart=always never fires because the process never exits.
# Fix: emit || exit 1 in the loop body.
#
# DEFECT 2. Both cursor marks absent is both "cold start" and "recovered from
# crash", and the code treated them identically. After a crash the watcher
# seeds at now and discards every answer that arrived during the outage.
# Fix: a witness file present while both marks are absent is recovery, not
# a cold start — emit one SEEDED line counting the waiting answers.
#
# covers: cockpit/watch-answers.sh spira/answers.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require test-answers-silent-seed
testdb_up answers-silent-seed || { echo "testdb_up failed"; exit 1; }

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

echo "test-answers-silent-seed.sh"

ASK_LABEL="needs-operator"
OPERATOR_ACTOR="testop"
bdc() { BEADS_ACTOR="$OPERATOR_ACTOR" "$SPIRA_BD" -C "$SPIRA_DB" "$@" 2>/dev/null; }

# One closed ask bead: represents an answer waiting in the DB.
AID=$(bdc create --title "should I proceed?" -l "$ASK_LABEL" --json 2>/dev/null \
    | sed -n '/^[[{]/,$p' \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("id",""))' \
    2>/dev/null) || AID=""
bdc close "$AID" --reason "yes, proceed" >/dev/null 2>&1 || true
if [ -z "$AID" ]; then
    echo "FAIL: could not create fixture bead"; exit 1
fi

BEAD_JSON=$("$SPIRA_BD" -C "$SPIRA_DB" list --all --limit 0 \
    --label-any "${ASK_LABEL},overseer,insight" --json 2>/dev/null \
    | sed -n '/^[[{]/,$p')

WITNESS="$TMP/witness.txt"
VCURSOR="$TMP/vcursor"
CCURSOR="$TMP/ccursor"
touch "$TMP/self-closed"

run_answers() {
    printf '%s' "$BEAD_JSON" | python3 "$HERE/answers.py" \
        "bd=$SPIRA_BD" "db=$SPIRA_DB" \
        "ask_label=$ASK_LABEL" \
        "operator_actor=$OPERATOR_ACTOR" \
        "operator=testop" \
        "verdict_cursor=$VCURSOR" "comment_cursor=$CCURSOR" \
        "witness=$WITNESS" \
        "self_closed=$TMP/self-closed" \
        "format=monitor" 2>&1
}

# -----------------------------------------------------------------------
echo
echo "defect 2: recovery seeding"

echo "cold start: no witness, both marks absent — silent (no SEEDED line)"
rm -f "$WITNESS" "$VCURSOR" "$CCURSOR"
out="$(run_answers)"
nowant "cold start produces no SEEDED line" "SEEDED" "$out"
[ -f "$VCURSOR" ] \
    && ok "cold start wrote cursor (positive control: answers.py ran)" \
    || bad "cold start wrote cursor (positive control)" "cursor not created"

echo "recovery: witness present, both marks absent — SEEDED line with count"
rm -f "$VCURSOR" "$CCURSOR"
printf '%s\n' "$AID" > "$WITNESS"
out="$(run_answers)"
want "recovery emits SEEDED AT line" "SEEDED AT" "$out"
want "SEEDED line reports 1 waiting answer" "1 answer" "$out"

echo "normal pass: witness present, marks advanced — no SEEDED line"
# Seed the marks to a past time so subsequent close is "new" in normal use.
printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}\n' > "$VCURSOR"
printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}\n' > "$CCURSOR"
printf '%s\n' "$AID" > "$WITNESS"
out="$(run_answers)"
nowant "normal pass with advanced marks produces no SEEDED line" "SEEDED" "$out"

# -----------------------------------------------------------------------
echo
echo "defect 1: loop exits non-zero on a failing pass"

# POSITIVE CONTROL: without || exit 1, a loop with a failing emit runs forever
# (timeout kills it at 124, not at 1).
unfixed_rc=0
timeout 0.3 bash -c \
    'emit() { return 1; }; INTERVAL=0; while true; do emit; sleep "$INTERVAL"; done' \
    2>/dev/null || unfixed_rc=$?
is "unfixed loop runs forever on failing emit (positive control: exit 124)" "124" "$unfixed_rc"

# THE FIX: || exit 1 causes the loop to exit 1 on the first failing pass.
fixed_rc=0
(
    emit() { return 1; }
    INTERVAL=0
    while true; do emit || exit 1; sleep "$INTERVAL"; done
) || fixed_rc=$?
is "fixed loop exits 1 on failing emit" "1" "$fixed_rc"

# BEHAVIORAL: run the actual watch-answers.sh loop. Force emit to fail by
# pointing ANSWER_STATE at a path whose parent is a regular file — Python's
# write_witness cannot open the .tmp file there (NotADirectoryError), exits
# non-zero, and with the fix the loop exits 1 before the timeout fires.
FAIL_PARENT="$TMP/not-a-dir"
touch "$FAIL_PARENT"           # regular file, not directory
FAIL_WITNESS="$FAIL_PARENT/witness.txt"
WATCH_SCRIPT="$HERE/../cockpit/watch-answers.sh"
loop_rc=0
timeout 5 bash -c "
    export SPIRA_DB='$SPIRA_DB'
    export COCKPIT_DB='$SPIRA_DB'
    export BD_BIN='$SPIRA_BD'
    export SPIRA_RUN='$TMP/wrun'
    export SPIRA_ASK_LABEL='$ASK_LABEL'
    export ANSWER_STATE='$FAIL_WITNESS'
    export ANSWER_POLL=0
    exec '$WATCH_SCRIPT' loop
" 2>/dev/null || loop_rc=$?
[ "$loop_rc" -eq 1 ] \
    && ok "watch-answers.sh loop exits 1 when a pass fails" \
    || bad "watch-answers.sh loop exits 1 when a pass fails" \
           "got exit $loop_rc (0=emit succeeded without failing, 124=looped forever)"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
