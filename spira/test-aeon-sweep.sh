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
# tier: T2
# covers: aeon/src/* spira/lib.sh UC-aeon-execution-05
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

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
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"

# Custom fayth: uses the test-label partition; no SOP_REQUIRED so the closing rule
# does not fire and confuse the attempt-label check.
FAYTH_LABELS_T="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}test-sweep-bead"
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
[ -x "${SPIRA_AEON_BIN:-}" ] \
    || { printf 'test-aeon-sweep: the aeon binary is not built (SPIRA_AEON_BIN) — refusing to run the real model\n' >&2; exit 1; }

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

aeon() { "$SPIRA_AEON_BIN" --home "$SPIRA_HOME" "$@" 2>/dev/null; }

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
# The stub agent exits 0, so a live probe would lift this pause. Mark the probe
# interval as just-run so the interval guard blocks the probe for this case.
printf '%s\n' "$(date +%s)" > "$SPIRA_RUN/capacity-probe-last"
rm -f "$LEDGER"

aeon testsweep --sweep --prompt "should not run"

want "born is still written"       "born testsweep" "$(cat "$LEDGER" 2>/dev/null)"
want "awake shows paused"          "awake testsweep paused" "$(cat "$LEDGER" 2>/dev/null)"
nowant "done is NOT written"       "done testsweep" "$(cat "$LEDGER" 2>/dev/null)"
rm -f "$CAP_FILE"; unset SPIRA_CAPACITY_PAUSE_FILE

# ======================================================================================
echo
echo "--sweep: the capacity check excludes the caller's own unit (sp-0hnm6):"
# ======================================================================================
# Since sp-0y2av, aeon_count lists live spira-aeon-<fayth>-* units — and a freshly
# summoned aeon's own transient unit already exists (systemd-run created it before this
# script ever ran), so a naive count sees it too. Left unexcluded, "1/1 at capacity"
# fires on the FIRST aeon of a fayth with FAYTH_MAX_CONCURRENT=1 (testsweep, like the
# groomer), and it never sweeps at all.
#
# AEON_OWN_UNIT (env override, same idiom as SPIRA_INCIDENT_UNIT) stands in for a real
# systemd-run session's unit name — no real --user session is needed to prove the count
# excludes it. SPIRA_SYSTEMCTL is the mock systemctl test-summon-fast-path.sh's section A
# already established for the same purpose.
cat > "$BIN/mock-systemctl" <<'MOCK'
#!/usr/bin/env bash
if [ "$2" = list-units ]; then
    glob="$3"
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        case "$line" in $glob) printf '%s\n' "$line" ;; esac
    done < "$MOCK_UNITS_FILE"
fi
MOCK
chmod +x "$BIN/mock-systemctl"
export SPIRA_SYSTEMCTL="$BIN/mock-systemctl"
export AEON_OWN_UNIT="spira-aeon-testsweep-selfunit.service"
MOCK_UNITS_FILE="$TMP/mock-units"; export MOCK_UNITS_FILE

# THE DEFECT ITSELF: the only live unit of this fayth is the caller's own. Today's code
# (aeon_count with no exclusion) counts it, sees 1/1, and refuses to sweep at all.
rm -f "$LEDGER"
printf '%s\n' "$AEON_OWN_UNIT" > "$MOCK_UNITS_FILE"

aeon testsweep --sweep --prompt "capacity self-count check"

want  "own unit excluded: the sweep proceeds" "awake testsweep sweep" "$(cat "$LEDGER" 2>/dev/null)"
want  "own unit excluded: teardown still runs" "done testsweep" "$(cat "$LEDGER" 2>/dev/null)"
nowant "own unit excluded: no capacity refusal" "awake testsweep capacity" "$(cat "$LEDGER" 2>/dev/null)"

# THE COMPANION CHECK: a second, genuinely OTHER live unit of the same fayth must still
# trip the cap — excluding the caller's own unit must not disable the check entirely.
rm -f "$LEDGER"
printf '%s\nspira-aeon-testsweep-other.service\n' "$AEON_OWN_UNIT" > "$MOCK_UNITS_FILE"

aeon testsweep --sweep --prompt "should not run: at capacity"

want   "one other live unit: capacity refusal fires" "awake testsweep capacity" "$(cat "$LEDGER" 2>/dev/null)"
nowant "one other live unit: no done line (never ran)" "done testsweep" "$(cat "$LEDGER" 2>/dev/null)"

unset SPIRA_SYSTEMCTL AEON_OWN_UNIT MOCK_UNITS_FILE
rm -f "$LEDGER"

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

# ======================================================================================
echo
echo "--sweep: a refused session (no tool calls, claude rc=1) exits non-zero:"
# ======================================================================================
# POSITIVE CONTROL for UC-aeon-execution-05's "exits non-zero if the API refused it": a
# refused sweep — no tool calls at all — must not be folded into the same "ran and did
# work" exit-0 path a stray non-zero tool-call rc gets (sweep_cleanup's session_outcome
# check). Rehomed from test-aeon-teardown-e2e.sh (sp-5t53s), cut there for the area's 60s cap.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"result","subtype":"error","is_error":true,"result":"you have reached your session limit","duration_ms":100,"num_turns":0,"total_cost_usd":0}\n'
exit 1
SHIM
chmod +x "$BIN/claude"

aeon testsweep --sweep --prompt "check pipeline"; rc=$?
is "a refused sweep exits non-zero (a real ops failure stays visible)" "1" "$rc"

# ======================================================================================
echo
echo "--sweep: launch carries --settings naming aeon-fence.sh when the hook is present:"
# ======================================================================================
# POSITIVE CONTROL: without aeon-fence.sh the settings JSON must NOT name it — this proves
# the assertion below can detect absence before trusting that it detects presence.
mkdir -p "$SPIRA_HOME/hooks"
rm -f "$SPIRA_HOME/hooks/aeon-fence.sh"

cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$TMP/claude-argv"
cat /dev/stdin > "$TMP/sweep-prompt"
printf '{"type":"result","subtype":"success","duration_ms":1,"turns":0,"num_turns":0,"total_cost_usd":0}\n'
SHIM
chmod +x "$BIN/claude"

rm -f "$TMP/claude-argv"
aeon testsweep --sweep --prompt "settings probe"

_s_argv="$(cat "$TMP/claude-argv" 2>/dev/null || true)"
want  "PC: --settings in argv even without fence" "--settings" "$_s_argv"
_s_json_nofence="$(awk '/^--settings$/{getline; print; exit}' "$TMP/claude-argv" 2>/dev/null || true)"
nowant "PC: settings JSON absent aeon-fence.sh before hook created" "aeon-fence.sh" "$_s_json_nofence"

# Install the fence hook and verify it appears in the settings JSON.
printf '#!/usr/bin/env bash\n' > "$SPIRA_HOME/hooks/aeon-fence.sh"
chmod +x "$SPIRA_HOME/hooks/aeon-fence.sh"

rm -f "$TMP/claude-argv"
aeon testsweep --sweep --prompt "settings probe with fence"

_s_argv2="$(cat "$TMP/claude-argv" 2>/dev/null || true)"
want "--settings in sweep argv" "--settings" "$_s_argv2"
_s_json_fence="$(awk '/^--settings$/{getline; print; exit}' "$TMP/claude-argv" 2>/dev/null || true)"
want "settings JSON names aeon-fence.sh" "aeon-fence.sh" "$_s_json_fence"

echo
tl_summary
