#!/usr/bin/env bash
#
# test-aeon-presession-death.sh — a worktree creation failure is named (git's own error
#                                  in the FATAL line), charges an attempt, and writes a
#                                  distinguishable ledger status (pre-session, not in_progress).
#
#   ./test-aeon-presession-death.sh
#
# THE DEFECT THIS GUARDS. An aeon that dies before its Claude session starts is recorded as
# a refusal (no attempt charged) and re-summoned forever: the ledger line reads rc=0
# status=in_progress with every metric ?, identical to a short successful run. Two P0 beads
# burned six summons in nine minutes with no work done and no alert.
#
# The root cause of that incident was a stray local branch refs/heads/origin/main making
# the string "origin/main" ambiguous to git worktree add. The broader defect is that ALL
# pre-session deaths share the same three flaws: the error is discarded, no attempt is
# charged, and the ledger line is indistinguishable from success.
#
# THREE THINGS ASSERTED:
#   1. A worktree failure names git's own error in the FATAL line.
#      Positive control: create refs/heads/origin/main in a FIXTURE repo and assert the
#      string "ambiguous" appears in the output.
#   2. Each pre-session death charges an attempt (ledger status=pre-session, not in_progress;
#      assert on the ledger, not on intent).
#   3. A second death on the same bead also charges an attempt — the ledger shows two
#      pre-session lines for the same bead, proving the infinite-loop is broken.
#
# Driven through the REAL aeon.sh against a real bd on a throwaway fixture, with a shim
# standing in for the model. The shim must never be called — it asserts as much. Testing
# via the real aeon.sh verifies the mechanism (law-prefer-the-real-dependency).
#
# defect: sp-ywlti
# covers: spira/aeon.sh spira/lib.sh
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
testdb_require test-aeon-presession-death
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonpsess || { echo "test-aeon-presession-death: could not build fixture"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# A "remote" with one commit on main, then a local clone that has refs/remotes/origin/main.
REMOTE="$TMP/remote"
git init -q -b main "$REMOTE"
git -C "$REMOTE" config user.email t@t; git -C "$REMOTE" config user.name t
printf 'seed\n' > "$REMOTE/f"; git -C "$REMOTE" add f; git -C "$REMOTE" commit -qm seed

REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" fetch -q origin 2>/dev/null
git -C "$REPO" checkout -q -b main --track origin/main 2>/dev/null \
    || git -C "$REPO" checkout -q main 2>/dev/null || true
git -C "$REPO" remote set-head origin --auto 2>/dev/null || true

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

# THE SHIM: never called (the aeon dies before the session starts), but must exist so
# that aeon.sh does not invoke the real model if the pre-session check misfires.
BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude"
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-presession-death: no SPIRA_AGENT in aeon.sh — refusing to run real model" >&2; exit 1; }
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf 'test-aeon-presession-death: shim was invoked — aeon did not die pre-session\n' >&2
exit 1
SHIM
chmod +x "$BIN/claude"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-23T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}

run_aeon() {
    rm -rf "$SPIRA_RUN/worktree"
    "$HERE/aeon.sh" builder > "$TMP/out" 2>&1
    echo $?
}

done_lines_for() {
    grep " done builder $1 " "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null
}

echo
echo "test-aeon-presession-death.sh"

# PLANT THE STRAY BRANCH that makes "origin/main" ambiguous to git worktree add.
# This replicates: git fetch /path/to/harness main:origin/main (production incident).
git -C "$REPO" branch "origin/main" main

# ======================================================================================
echo
echo "CASE 1 (positive control): worktree fails — FATAL names git's error, attempt charged:"
# ======================================================================================
testdb_reset; seed sp-pd-1
_rc="$(run_aeon)"
_out="$(cat "$TMP/out")"
_line="$(done_lines_for sp-pd-1 | tail -1)"

want "FATAL line is present"                "FATAL"             "$_out"
want "FATAL names git's own error (ambiguous)" "ambiguous"      "$_out"
want "ledger status is pre-session"         "status=pre-session" "$_line"
nowant "not recorded as in_progress"        "status=in_progress" "$_line"
nowant "rc is not 0 (session never ran)"    "rc=0 "              "$_line"

# ======================================================================================
echo
echo "CASE 2: second death on same bead — second attempt is also charged (ledger-based):"
# ======================================================================================
# The bead is back in open state after cleanup released it. Run again — the stray branch
# is still there, so the worktree fails again.
_rc2="$(run_aeon)"
_line2="$(done_lines_for sp-pd-1 | tail -1)"
_count="$(done_lines_for sp-pd-1 | grep -c 'status=pre-session' 2>/dev/null || echo 0)"

want "second ledger also shows pre-session"  "status=pre-session"  "$_line2"
is   "two pre-session entries in ledger (infinite loop broken)" "2" "$_count"

# ======================================================================================
echo
echo "POSITIVE CONTROL — stray branch removed: session runs, no pre-session death:"
# ======================================================================================
git -C "$REPO" branch -D "origin/main" 2>/dev/null || true

# Replace shim with one that emits valid session output (no close — we assert on session start,
# not on bead disposition, which belongs to aeon.sh's own cleanup path).
cat > "$BIN/claude" <<'SHIM2'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
SHIM2
chmod +x "$BIN/claude"

testdb_reset; seed sp-pd-3
_rc3="$(run_aeon)"
_out3="$(cat "$TMP/out")"
_line3="$(done_lines_for sp-pd-3 | tail -1)"

nowant "no FATAL when worktree succeeds"              "FATAL"             "$_out3"
nowant "no pre-session death in positive control"     "status=pre-session" "$_line3"
want   "session ran (turns recorded in ledger)"       "turns=1"           "$_line3"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
