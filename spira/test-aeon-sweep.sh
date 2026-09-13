#!/usr/bin/env bash
#
# test-aeon-sweep.sh — aeon.sh --sweep runs the persona without a bead.
#
#   ./test-aeon-sweep.sh
#
# WHAT IS UNDER TEST
# ------------------
# aeon.sh --sweep [--prompt <text>|-] summons a persona session with no bead: no claim,
# no lease, no close, no attempt counter. What DOES still apply is: the capacity check,
# the draining check, the concurrency cap, and the born/awake/done ledger lines (so
# cockpit counts work and a stillborn sweep shows as born-without-awake).
#
# THE POSITIVE CONTROL IS MANDATORY (law-prove-the-test-fails-without-the-fix). Before
# asserting that --sweep leaves no attempt label, the suite runs WITHOUT --sweep against
# the same bead and confirms an attempt label IS produced — proving the test can tell the
# difference. An assertion that merely checks "no label" passes either way if the checking
# path is broken; the positive control catches that.
#
# DRIVEN THROUGH THE REAL aeon.sh against a real bd on a throwaway fixture, with a shim
# standing in for the model. The guard that refuses to run the real model when the shim is
# absent is explicit: conf.sh replaces $PATH, so a PATH-only shim would run the real model
# at full cost.
#
# defect: sp-2tbr
# covers: spira/aeon.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-sweep
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonsweep || { echo "test-aeon-sweep: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t \
       GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# A bare origin and a clone for the aeon's worktree (needed by non-sweep path).
ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed
git -C "$REPO" push -q origin main 2>/dev/null

# Minimal harness layout in $TMP.
export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"

# Custom fayth: uses the test-label partition; no SOP_REQUIRED so the closing rule
# does not fire and confuse the attempt-label check.
FAYTH_LABELS_T="spira,test-sweep-bead"
cat > "$SPIRA_HOME/chamber/testsweep.fayth" <<FAYTH
FAYTH_NAME=testsweep
FAYTH_LABELS="$FAYTH_LABELS_T"
FAYTH_EXCLUDE_LABELS="spira-poison,\$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} on {{BRANCH}}\n{{PARK}}\n' \
    > "$SPIRA_HOME/chamber/testsweep.md"

# Mock claude binary. THE GUARD IS NOT DECORATION: conf.sh replaces $PATH, so a PATH
# shim would reach the real model through the replaced PATH and run it at full cost.
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { printf 'test-aeon-sweep: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model\n' >&2; exit 1; }

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
# The mock emits a tool_use + result event so that session_outcome classifies it as
# `unlanded` (the outcome that charges an attempt). Without the tool_use, the session
# looks like a refusal, which does NOT charge — and the positive control would not fire.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/sweep-prompt"
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}\n'
printf '{"type":"result","subtype":"success","duration_ms":1,"turns":1,"num_turns":1,"total_cost_usd":0}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

aeon() { bash "$SPIRA_HOME/aeon.sh" "$@" 2>/dev/null; }

# bd helpers against the fixture database.
bead_status()   { BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); d=r[0] if isinstance(r,list) else r; print(d.get("status","?"))'; }
bead_labels()   { BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); d=r[0] if isinstance(r,list) else r; print(",".join(d.get("labels",[]))  )'; }
any_attempt()   {  # any_attempt -> 1 if any bead carries an sp-attempt label
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" list --json 2>/dev/null \
        | python3 -c '
import json,sys
try: rows = json.load(sys.stdin)
except Exception: rows=[]
if not isinstance(rows, list): rows=[rows]
for r in rows:
    for l in (r.get("labels") or []):
        if l.startswith("sp-attempt"):
            print(l); sys.exit(0)
sys.exit(1)' 2>/dev/null
}

# Create the positive-control bead first. The positive control must run BEFORE any
# assertion it backs — a control placed after the assertion it supports cannot validate
# it; if it fails last, the preceding 'no attempt label' assertion has already been
# accepted as meaningful when it is not.
BID_PC="$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" create "positive control bead" --type task \
    -l "$FAYTH_LABELS_T,repo:fixture" 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+' | head -1)"
[ -n "$BID_PC" ] || { printf 'test-aeon-sweep: could not create positive control bead\n' >&2; exit 1; }

echo "test-aeon-sweep.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL — without --sweep a bead IS claimed and an attempt IS charged:"
# ======================================================================================
# Claims BID_PC with a non-sweep aeon. The mock exits without closing the bead so
# teardown charges an attempt (sp-attempt-N). This proves the machinery that would add an
# attempt label to BID below — if the aeon actually claimed it — is operational. Without
# this confirmed first, the 'no attempt label on sweep bead' assertion below cannot
# distinguish correct sweep behaviour from a broken attempt-charging path.
aeon testsweep

# sp-attempt-N labels are no longer written (sp-lzt); the events trail records claims.
# The discriminating fact is the 'branch:' state-label: aeon.sh calls
# 'bdq set-state <id> branch=spira/<id>' when it takes the branch, which bd turns into
# a 'branch:spira/<id>' label that persists through teardown. Sweep never claims, so
# the sweep bead never gets a branch label — that is the assertion below.
labels_pc="$(bead_labels "$BID_PC" 2>/dev/null || true)"
[[ "$labels_pc" == *"branch:"* ]] \
    && ok  "without --sweep a branch label IS added ($(printf '%s' "$labels_pc" | grep -oE 'branch:[^,]*' | head -1))" \
    || bad "without --sweep a branch label IS added" "none found on BID_PC labels=[$labels_pc]"

# BID_PC now carries a branch label. Sweep assertions below are scoped to BID
# (not the whole database) because BID_PC's labels would otherwise confound them.
BID="$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" create "sweep test incident" --type task \
    -l "$FAYTH_LABELS_T,repo:fixture" 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+' | head -1)"
[ -n "$BID" ] || { printf 'test-aeon-sweep: could not create bead\n' >&2; exit 1; }

# ======================================================================================
echo
echo "--sweep: born/awake/done ledger lines are written:"
# ======================================================================================
LEDGER="$SPIRA_RUN/aeon-ledger.log"

aeon testsweep --sweep --prompt "vital signs: all green"

want "born line written"  "born testsweep" "$(cat "$LEDGER" 2>/dev/null)"
want "awake sweep line"   "awake testsweep sweep" "$(cat "$LEDGER" 2>/dev/null)"
want "done sweep line"    "done testsweep sweep"  "$(cat "$LEDGER" 2>/dev/null)"

# ======================================================================================
echo
echo "--sweep: no bead is claimed and no branch label is set:"
# ======================================================================================
# THE CLAIM CHECK: the bead must still be open. An aeon that claimed it would move it to
# in_progress, then release it on teardown — and set the branch label (which persists).
is "bead stays open after sweep"  "open" "$(bead_status "$BID")"

# THE BRANCH-LABEL CHECK: sweep must not set a branch label on BID. Scoped to BID
# (not any_attempt across all beads) because BID_PC carries a branch label from the
# positive control above. If sweep incorrectly claimed BID, it would set 'branch:spira/<id>'
# — proving the positive control's discriminating fact is what this catches.
labels_bid="$(bead_labels "$BID" 2>/dev/null || true)"
nowant "no branch label on sweep bead" "branch:" "$labels_bid"

# ======================================================================================
echo
echo "--sweep: the model received the prompt:"
# ======================================================================================
want "prompt reached the model" "vital signs: all green" \
     "$(cat "$TMP/sweep-prompt" 2>/dev/null)"

# ======================================================================================
echo
echo "--sweep: capacity-paused sweeps are rejected (capacity still applies):"
# ======================================================================================
# Simulate a capacity pause by writing the marker file.
CAP_FILE="${SPIRA_RUN}/capacity-pause"
printf '9999999999\n' > "$CAP_FILE"
export SPIRA_CAPACITY_PAUSE_FILE="$CAP_FILE"
rm -f "$LEDGER"

aeon testsweep --sweep --prompt "should not run"

want "born is still written"       "born testsweep" "$(cat "$LEDGER" 2>/dev/null)"
want "awake shows paused"          "awake testsweep paused" "$(cat "$LEDGER" 2>/dev/null)"
nowant "done is NOT written"       "done testsweep" "$(cat "$LEDGER" 2>/dev/null)"
rm -f "$CAP_FILE"; unset SPIRA_CAPACITY_PAUSE_FILE

# ======================================================================================
echo
echo "--sweep: draining world rejects the sweep:"
# ======================================================================================
rm -f "$LEDGER"
printf 'draining\n' > "$SPIRA_RUN/world.draining"

aeon testsweep --sweep --prompt "should not run"

want "born is still written"       "born testsweep" "$(cat "$LEDGER" 2>/dev/null)"
want "awake shows draining"        "awake testsweep draining" "$(cat "$LEDGER" 2>/dev/null)"
rm -f "$SPIRA_RUN/world.draining"
rm -f "$LEDGER"

# ======================================================================================
echo
echo "--sweep -: prompt from stdin:"
# ======================================================================================
rm -f "$TMP/sweep-prompt"
printf 'stdin sweep prompt content' | aeon testsweep --sweep -
want "stdin prompt reached model" "stdin sweep prompt content" \
     "$(cat "$TMP/sweep-prompt" 2>/dev/null)"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
