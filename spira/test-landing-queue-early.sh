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
# A queue-mode branch is certified instantly (no local gate — law-a-round-takes-
# certified-tips), so ordering is read off the pass's own log rather than a gate delay:
# the "certified spira/<id>" line is land_repo's own marker that the branch loop reached
# this branch. The queue step is stubbed to control what the forge reports.
#
# tier: T2
# covers: landing-pass/* spira/lib.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
# THE LANDING PASS IS THE landing-pass BINARY (landing-pass/DESIGN.md §7.4): `land` for a
# pass, `halt` to stop one. Resolved from this tree before any fixture repoints SPIRA_REPO.
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
testdb_up landing-queue-early || {
    printf 'SKIP test-landing-queue-early: testdb not available\n' >&2
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
lc_path_stub "$SH" "$TMP/lcfix"

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub mail       '[ "${1:-}" = send ] || exit 0'
stub skew        'exit 0'
stub gh         'exit 1'

# VERDICT STUB: stateful — outputs a landing line exactly once (simulates a batch file that
# disappears after a fast-forward). After the first call the batch is gone; subsequent calls
# are silent.
# THE QUEUE STEP IS `queue step <repo>` now (landing-pass/DESIGN.md §8 D5): the
# stub stands in for the queue binary and runs verdict-fixture (the verdict runs in process
# inside `queue step` now, queue/DESIGN-verdict.md), which is all this suite's ordering
# assertions read.
stub queue 'if [ "${1:-}" = step ]; then verdict-fixture "${2:-}"; fi; exit 0'

B() { bd -C "$SPIRA_DB" "$@"; }

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
        SPIRA_GATE_WORKER=0 PATH="$SH:$PATH" landing-pass land 2>&1
}

seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

branch() {
    local id="$1"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-epic","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}

# repo-map in queue mode so the early and late queue checks run.
cat > "$SH/repo-map" <<MAP
$REPONAME | $REPO | queue | |
MAP

echo "test-landing-queue-early.sh"

# POSITIVE CONTROL: prove that a landing-pass crash is caught, not a silent non-zero exit.
# Put a landing-pass that exits 255 first on PATH (the SUT is found by name), run it.
mkdir -p "$TMP/crash"
printf '#!/usr/bin/env bash\nexit 255\n' > "$TMP/crash/landing-pass"; chmod +x "$TMP/crash/landing-pass"
_ctrl_out="$(PATH="$TMP/crash:$PATH" landing)"; _ctrl_rc=$?
[ "$_ctrl_rc" -ne 0 ] \
    && ok "positive-control: landing crash detected (rc=$_ctrl_rc)" \
    || bad "positive-control" "expected non-zero from a landing-pass that exits 255; got rc=0"

# --------------------------------------------------------------------------------------
# CASE 1 (early): forge is green before the pass starts.
# The batch lands at the early check, before the gate runs.
# --------------------------------------------------------------------------------------

# verdict-fixture stub: first call reports green and marks the batch consumed; subsequent calls are
# silent (the batch file is gone after a fast-forward, so the real verdict would also be
# silent on the second call).
stub verdict-fixture '
[ -f "'"$RUN"'/batch-landed" ] && exit 0
touch "'"$RUN"'/batch-landed"
printf "verdict fixture: PR 1 landed by fast-forward (abc123)\n"'

seed
branch sp-earlyq
rm -f "$RUN/batch-landed"
out="$(landing)"; _landing_rc=$?
[ "$_landing_rc" -eq 0 ] || bad "1. landing-crashed" "landing-pass land exited $_landing_rc (a stub or subprocess died)"

want "1. early green: early check logs a landing" "queue early: verdict fixture: PR 1 landed by fast-forward" "$out"
nowant "1. early green: late check is not where it landed" "queue late: verdict fixture: PR 1 landed by fast-forward" "$out"
before_in_output "1. early green: verdict ran before the branch loop reached sp-earlyq" \
    "queue early: verdict fixture: PR 1 landed by fast-forward" "certified spira/sp-earlyq" "$out"

git -C "$REPO" worktree remove --force "$RUN/worktree/sp-earlyq" 2>/dev/null || true
git -C "$REPO" branch -D "spira/sp-earlyq" 2>/dev/null || true

# --------------------------------------------------------------------------------------
# CASE 2 (late): forge is pending at the top of the pass, green by the end.
# The batch lands at the late check, after the gate runs.
# --------------------------------------------------------------------------------------

# verdict-fixture stub: pending until sp-lateq is certified (land_repo's own marker file),
# then reports landing on the next call. This ties "late" to the branch loop having
# actually run, rather than to a call count tuned around a gate delay that no longer
# exists (queue-mode branches certify with no local gate — law-a-round-takes-
# certified-tips).
stub verdict-fixture '
if [ -f "'"$RUN"'/landstate/sp-lateq" ] && grep -q "^CERTIFIED" "'"$RUN"'/landstate/sp-lateq"; then
    printf "verdict fixture: PR 1 landed by fast-forward (abc456)\n"
fi'

seed
branch sp-lateq
rm -f "$RUN/landstate/sp-lateq"
out="$(landing)"; _landing_rc=$?
[ "$_landing_rc" -eq 0 ] || bad "2. landing-crashed" "landing-pass land exited $_landing_rc (a stub or subprocess died)"

nowant "2. late green: early check does not show a landing" "queue early: verdict fixture: PR 1 landed by fast-forward" "$out"
want   "2. late green: late check logs a landing"           "queue late: verdict fixture: PR 1 landed by fast-forward" "$out"
before_in_output "2. late green: branch loop reached sp-lateq before the late verdict" \
    "certified spira/sp-lateq" "queue late: verdict fixture: PR 1 landed by fast-forward" "$out"

git -C "$REPO" worktree remove --force "$RUN/worktree/sp-lateq" 2>/dev/null || true
git -C "$REPO" branch -D "spira/sp-lateq" 2>/dev/null || true
tl_summary
