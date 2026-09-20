#!/usr/bin/env bash
#
# test-landing-queue-early.sh — a landing pass checks the queue at the top of the pass so a
# batch whose CI went green before the gates run lands immediately, not after the gate sequence.
#
#   ./test-landing-queue-early.sh
#
# THE CASE: a queue-mode batch whose forge reports green before the pass starts waits for every
# gate the pass selects — up to 90 minutes — before the queue step runs. The early check runs
# queue.sh step before any gate, so a green batch lands in seconds.
#
# WHAT IS TESTED:
#   1. (early) Forge is green before the pass starts: the batch lands at the early check, before
#      the gate runs. The log names the early check ("queue early:").
#   2. (late)  Forge is pending at the start, green by the end: the batch lands at the late
#      check, after the gate. The log names the late check ("queue late:").
#
# gate.sh is stubbed to sleep briefly and write a marker so the test can determine when the
# gate ran relative to the queue step output. verdict.sh and batch.sh are stubbed to control
# what the forge reports.
#
# covers: spira/landing.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
before_in_output() {
    # before_in_output <label> <first> <second> <output>
    # passes if <first> appears before <second> in <output>
    local label="$1" first="$2" second="$3" out="$4"
    local pos_first pos_second
    pos_first="$(printf '%s' "$out" | grep -n "$first" | head -1 | cut -d: -f1)"
    pos_second="$(printf '%s' "$out" | grep -n "$second" | head -1 | cut -d: -f1)"
    if [ -z "$pos_first" ] || [ -z "$pos_second" ]; then
        bad "$label" "one or both patterns not found (first=$first, second=$second)"
    elif [ "$pos_first" -lt "$pos_second" ]; then
        ok "$label"
    else
        bad "$label" "[$first] (line $pos_first) did not appear before [$second] (line $pos_second)"
    fi
}

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-queue-early
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up landing-queue-early || {
    printf 'SKIP test-landing-queue-early: server testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
REPONAME=fixture-queue
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"

cp "$HERE"/*.sh "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub mail.sh    '[ "${1:-}" = send ] || exit 0'
stub skew.sh    'exit 0'
stub gh         'exit 1'

# GATE STUB: writes a "gate-ran" marker to a control file so we can verify ordering,
# then sleeps briefly to simulate a non-zero gate cost. Passes unconditionally.
stub gate.sh '
printf "gate-ran\n" >> "'"$RUN"'/order-log"
sleep 1
echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2
exit 0'

# VERDICT STUB: reads its control file to decide whether to report green or pending.
#   $RUN/forge-green exists → output a "landed" line and write "verdict-ran:early" marker
#   otherwise → print nothing (pending)
# batch.sh is a no-op (no new batch to open in this fixture).
stub batch.sh 'exit 0'

B() { bd -C "$SPIRA_DB" "$@"; }

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
        bash "$SH/landing.sh" 2>&1
}

seed() {
    testdb_reset
    : > "$RUN/order-log"
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

branch() {
    local id="$1"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}

# repo-map in queue mode so the early and late queue checks run.
cat > "$SH/repo-map" <<MAP
$REPONAME | $REPO | queue | |
MAP

echo "test-landing-queue-early.sh"

# --------------------------------------------------------------------------------------
# CASE 1 (early): forge is green before the pass starts.
# The batch lands at the early check, before the gate runs.
# --------------------------------------------------------------------------------------

# verdict.sh stub: always reports a landing (forge is green).
stub verdict.sh '
printf "verdict fixture: PR 1 landed by fast-forward (abc123)\n"
printf "verdict-ran\n" >> "'"$RUN"'/order-log"'

seed
branch sp-earlyq
: > "$RUN/order-log"
out="$(landing)"

want "1. early green: early check logs a landing" "queue early: verdict fixture: PR 1 landed by fast-forward" "$out"
nowant "1. early green: late check is not where it landed" "queue late: verdict fixture: PR 1 landed by fast-forward" "$out"
before_in_output "1. early green: verdict ran before the gate" "verdict-ran" "gate-ran" "$(cat "$RUN/order-log")"

git -C "$REPO" worktree remove --force "$RUN/worktree/sp-earlyq" 2>/dev/null || true
git -C "$REPO" branch -D "spira/sp-earlyq" 2>/dev/null || true

# --------------------------------------------------------------------------------------
# CASE 2 (late): forge is pending at the top of the pass, green by the end.
# The batch lands at the late check, after the gate runs.
# --------------------------------------------------------------------------------------

# verdict.sh stub: first call (early) is pending (no output); second call (late) reports landing.
# The VERDICT_CALL_COUNT file tracks how many times verdict.sh has been called.
rm -f "$RUN/verdict-call-count"
stub verdict.sh '
count=0
[ -f "'"$RUN"'/verdict-call-count" ] && count="$(cat "'"$RUN"'/verdict-call-count")"
count=$(( count + 1 ))
printf "%d\n" "$count" > "'"$RUN"'/verdict-call-count"
printf "verdict-ran\n" >> "'"$RUN"'/order-log"
if [ "$count" -ge 2 ]; then
    printf "verdict fixture: PR 1 landed by fast-forward (abc456)\n"
fi'

seed
branch sp-lateq
: > "$RUN/order-log"
rm -f "$RUN/verdict-call-count"
out="$(landing)"

nowant "2. late green: early check does not show a landing" "queue early: verdict fixture: PR 1 landed by fast-forward" "$out"
want   "2. late green: late check logs a landing"           "queue late: verdict fixture: PR 1 landed by fast-forward" "$out"
before_in_output "2. late green: gate ran before the late verdict" "gate-ran" "verdict-ran" "$(cat "$RUN/order-log" | tail -2)"

git -C "$REPO" worktree remove --force "$RUN/worktree/sp-lateq" 2>/dev/null || true
git -C "$REPO" branch -D "spira/sp-lateq" 2>/dev/null || true

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
