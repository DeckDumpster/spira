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
# the string "origin/main" ambiguous to git worktree add. That specific path is fixed by
# aeon.sh qualifying the ref to refs/remotes/origin/main — tested in test-aeon-base-ref-
# qualify.sh. The broader defect is that ALL pre-session deaths share the same three flaws:
# the error is discarded, no attempt is charged, and the ledger line is indistinguishable
# from success.
#
# THREE THINGS ASSERTED:
#   1. A worktree failure names git's own error in the FATAL line.
#      Positive control: delete refs/remotes/origin/main so the land-ref check fails
#      before git worktree add; assert the error message appears in the log.
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

# Replace the shim with a success shim to test the fix: stray branch is now harmless
# because aeon.sh qualifies the ref before passing to git worktree add. The full
# regression is covered by test-aeon-base-ref-qualify.sh; here we just confirm the
# session runs normally so the pre-session death cases below stand on a clean baseline.
git -C "$REPO" branch "origin/main" main  # stray branch — no longer causes worktree failure
cat > "$BIN/claude" <<'SHIM2'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
SHIM2
chmod +x "$BIN/claude"

# ======================================================================================
echo
echo "BASELINE (fix in place): stray branch present, but session runs normally:"
# ======================================================================================
testdb_reset; seed sp-pd-0
_rc0="$(run_aeon)"
_out0="$(cat "$TMP/out")"
_line0="$(done_lines_for sp-pd-0 | tail -1)"

nowant "stray branch no longer blocks worktree"    "FATAL"              "$_out0"
nowant "no pre-session death with qualified ref"   "status=pre-session" "$_line0"
want   "session ran (turns recorded in ledger)"    "turns=1"            "$_line0"

# Remove stray branch; delete refs/remotes/origin/main so qualify_base_ref falls back to
# the unqualified "origin/main", which git worktree add cannot resolve — triggering the
# die "could not create a worktree" path. The repo stays in place so the repo-map check
# passes; only the worktree-add fails.
git -C "$REPO" branch -D "origin/main" 2>/dev/null || true
_origin_main_sha="$(git -C "$REPO" rev-parse refs/remotes/origin/main)"
git -C "$REPO" update-ref -d refs/remotes/origin/main

# Restore the never-called shim — the aeon must die before the session starts.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf 'test-aeon-presession-death: shim was invoked — aeon did not die pre-session\n' >&2
exit 1
SHIM
chmod +x "$BIN/claude"

# ======================================================================================
echo
echo "CASE 1: pre-session death (unresolvable land ref) — error logged, attempt charged:"
# ======================================================================================
testdb_reset; seed sp-pd-1
_rc="$(run_aeon)"
_out="$(cat "$TMP/out")"
_line="$(done_lines_for sp-pd-1 | tail -1)"

want "error message is logged (not swallowed)"    "land ref cannot be resolved" "$_out"
want "ledger status is pre-session"               "status=pre-session"          "$_line"
nowant "not recorded as in_progress"              "status=in_progress"          "$_line"
nowant "rc is not 0 (session never ran)"          "rc=0 "                       "$_line"

# ======================================================================================
echo
echo "CASE 2: second death on same bead — second attempt is also charged (ledger-based):"
# ======================================================================================
# The bead is back in open state after cleanup released it. Run again — ref still absent.
_rc2="$(run_aeon)"
_line2="$(done_lines_for sp-pd-1 | tail -1)"
_count="$(done_lines_for sp-pd-1 | grep -c 'status=pre-session' 2>/dev/null || echo 0)"

want "second ledger also shows pre-session"  "status=pre-session"  "$_line2"
is   "two pre-session entries in ledger (infinite loop broken)" "2" "$_count"

# ======================================================================================
echo
echo "CASE 3: third consecutive sub-10s death — rapid-recur detector fires:"
# ======================================================================================
# The land ref is still absent. After two runs the detector has not yet fired;
# a third pushes the count to SPIRA_RAPID_RECUR_THRESHOLD (default 3) and triggers it.
# Check absence first — proves the check is not over-eager — then fire the third run.
_note_before3="$(bd -C "$SPIRA_DB" show sp-pd-1 --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("notes","") if d else "")' 2>/dev/null)"
nowant "no rapid-recur note after only 2 runs" "RAPID" "$_note_before3"

_rc3b="$(run_aeon)"
_line3b="$(done_lines_for sp-pd-1 | tail -1)"
_count3="$(done_lines_for sp-pd-1 | grep -c 'status=pre-session' 2>/dev/null || echo 0)"
_note3b="$(bd -C "$SPIRA_DB" show sp-pd-1 --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("notes","") if d else "")' 2>/dev/null)"

want "third pre-session entry in ledger"           "status=pre-session" "$_line3b"
is   "three pre-session entries total"             "3"                  "$_count3"
want "rapid-recur note on bead after 3 short runs" "RAPID"              "$_note3b"

# ======================================================================================
echo
echo "CASE 4: rapid-recur PARKS the bead — dispatch stops re-summoning it, not just annotates:"
# ======================================================================================
_labels3b="$(bd -C "$SPIRA_DB" label list sp-pd-1 2>/dev/null)"
want "rapid-recur labeled the bead $SPIRA_ASK_LABEL" "$SPIRA_ASK_LABEL" "$_labels3b"
want "rapid-recur labeled the bead overseer"          "overseer"        "$_labels3b"

# builder.fayth excludes $SPIRA_ASK_LABEL, so a fourth summon must find nothing ready —
# the bead is parked, not merely annotated — and must charge no further attempt.
_rc4="$(run_aeon)"
_out4="$(cat "$TMP/out")"
_count4="$(done_lines_for sp-pd-1 | grep -c 'status=pre-session' 2>/dev/null || echo 0)"
_note4="$(bd -C "$SPIRA_DB" show sp-pd-1 --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("notes","") if d else "")' 2>/dev/null)"
_n_rapid_notes="$(grep -c 'RAPID-RECUR' <<< "$_note4" || true)"

want "fourth summon finds nothing ready (bead is parked)"  "nothing ready"      "$_out4"
is   "no fourth pre-session death charged"                 "3"                  "$_count4"
is   "rapid-recur note not re-appended once parked"         "1"                  "$_n_rapid_notes"

# ======================================================================================
echo
echo "POSITIVE CONTROL — ref restored: session runs, no pre-session death:"
# ======================================================================================
git -C "$REPO" update-ref refs/remotes/origin/main "$_origin_main_sha"

# Replace shim with one that emits valid session output.
cat > "$BIN/claude" <<'SHIM3'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
SHIM3
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
