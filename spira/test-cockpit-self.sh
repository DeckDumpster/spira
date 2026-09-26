#!/usr/bin/env bash
#
# test-cockpit-self.sh — the SELF row shows what is repeating now, and tripwires only when they trip.
#
#   ./test-cockpit-self.sh
#
# WHAT THIS SUITE EXISTS FOR
# --------------------------
# cockpit-metrics.py now emits short-window SELF metrics:
#
#   SP_SELF_REPEATING_N    count of ACT texts repeating in consecutive passes right now
#   SP_SELF_REPEATING{i}   formatted: "act text × N passes (Dm)"
#   SP_SELF_STILLBORN_W    aeons born but not awake within the window (died-at-birth count)
#   SP_SELF_STILLBORN_LAST "Nm ago" for the last such event
#   SP_SELF_STARVED_W      stalled passes within the window
#   SP_SELF_STARVED_LAST   "Nm ago" for the last one
#
# And health.sh renders them:
#   REPEATING rows only when SP_SELF_REPEATING_N > 0
#   BIRTH alert only when SP_SELF_STILLBORN_W > 0
#   STALL alert only when SP_SELF_STARVED_W > 0
#   JUDGE (passes since judgement) always
#
# ACCEPTANCE CRITERIA (from the bead):
#   1. A fixture sentinel.log with a burst 6h ago and nothing since renders no SELF row
#      (no REPEATING output from cockpit-metrics.py; SP_SELF_REPEATING_N=0)
#   2. A fixture with three consecutive "handled 1 stranded item(s)" in the last ten
#      minutes renders SP_SELF_REPEATING_N=1 and one formatted REPEATING string
#   3. A ledger with one stillborn aeon in-window renders SP_SELF_STILLBORN_W=1
#      and a ledger with one out-of-window renders SP_SELF_STILLBORN_W=0
#
# EVERY CHECK HAS A POSITIVE CONTROL. A suite that only tests "nothing found" is
# indistinguishable from one pointed at the wrong place; the positive control is a fixture
# that SHOULD trigger the check, proven first (law-absence-needs-a-positive-control).
#
# NO DATABASE, NO BOX. The SELF metrics are pure functions over log lines, so the fixtures
# are strings and the suite is hermetic by construction.
#
# defect: sp-jo8i
# covers: spira/cockpit-metrics.py cockpit/health.sh

# covers: spira/cockpit-metrics.py cockpit/health.sh
# scar: the SELF metrics (repeating aeons, stillborn starts, starved passes) were absent; current session anomalies were invisible on the health pane.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
HEALTH="$(cd "$(dirname "$0")/../cockpit" && pwd)/health.sh"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Call cockpit-metrics.py in a clean environment, using strings as fixture files.
# The window for SELF metrics is set to 90 minutes via SPIRA_SELF_WINDOW so that "last
# 10 minutes" is clearly inside and "6 hours ago" is clearly outside.
run_metrics() {   # run_metrics <sentinel-fixture-string> <ledger-fixture-string>
    local sf="$TMP/sent.log" lf="$TMP/ledger.log"
    printf '%s' "$1" > "$sf"
    printf '%s' "$2" > "$lf"
    env -i PATH="/usr/bin:/bin" HOME="$TMP" PYTHONPATH="$HERE" \
        SPIRA_SELF_WINDOW=90 \
        python3 "$HERE/cockpit-metrics.py" "$sf" "$lf" 24 2>/dev/null
}

# Timestamps used in fixtures. "now" is computed once so fixtures are consistent.
NOW_EPOCH=$(date +%s)
# Ten minutes ago — inside the 90-min window.
TS_10M=$(date -u -d "@$(( NOW_EPOCH - 600 ))" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null \
         || date -u -r $(( NOW_EPOCH - 600 )) '+%Y-%m-%dT%H:%M:%SZ')
TS_8M=$(date -u -d "@$(( NOW_EPOCH - 480 ))" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null \
        || date -u -r $(( NOW_EPOCH - 480 )) '+%Y-%m-%dT%H:%M:%SZ')
TS_6M=$(date -u -d "@$(( NOW_EPOCH - 360 ))" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null \
        || date -u -r $(( NOW_EPOCH - 360 )) '+%Y-%m-%dT%H:%M:%SZ')
TS_2M=$(date -u -d "@$(( NOW_EPOCH - 120 ))" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null \
        || date -u -r $(( NOW_EPOCH - 120 )) '+%Y-%m-%dT%H:%M:%SZ')
# Six hours ago — outside the 90-min window.
TS_6H=$(date -u -d "@$(( NOW_EPOCH - 21600 ))" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null \
        || date -u -r $(( NOW_EPOCH - 21600 )) '+%Y-%m-%dT%H:%M:%SZ')
TS_6H2=$(date -u -d "@$(( NOW_EPOCH - 21540 ))" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null \
         || date -u -r $(( NOW_EPOCH - 21540 )) '+%Y-%m-%dT%H:%M:%SZ')
TS_6H3=$(date -u -d "@$(( NOW_EPOCH - 21480 ))" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null \
         || date -u -r $(( NOW_EPOCH - 21480 )) '+%Y-%m-%dT%H:%M:%SZ')

EMPTY_LEDGER=""

# ======================================================================================
echo "ACCEPTANCE 1: burst 6h ago and nothing since → SP_SELF_REPEATING_N=0"

# THE POSITIVE CONTROL: same fixture structure but within the window, to prove the
# parser can find repeating acts when they exist.
REPEAT_IN_WINDOW="
${TS_10M} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_10M} spira: ACT handled 1 stranded item(s)
${TS_8M} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_8M} spira: ACT handled 1 stranded item(s)
${TS_6M} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_6M} spira: ACT handled 1 stranded item(s)
"
out_pos="$(run_metrics "$REPEAT_IN_WINDOW" "$EMPTY_LEDGER")"
# SP_SELF_REPEATING_N is the count of DISTINCT repeating patterns, not the run length.
# One unique ACT text repeating → N=1.
want "positive control: SP_SELF_REPEATING_N > 0 when in window" "SP_SELF_REPEATING_N=1" "$out_pos"
want "positive control: SP_SELF_REPEATING0 present"             "SP_SELF_REPEATING0="   "$out_pos"
want "positive control: repeating string contains act text"     "handled 1 stranded"    "$out_pos"
want "positive control: repeating string contains count"        "3 passes"              "$out_pos"

# THE BURST 6H AGO: same act text but all passes outside the 90-min window.
BURST_6H_AGO="
${TS_6H} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_6H} spira: ACT handled 1 stranded item(s)
${TS_6H2} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_6H2} spira: ACT handled 1 stranded item(s)
${TS_6H3} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_6H3} spira: ACT handled 1 stranded item(s)
"
out_6h="$(run_metrics "$BURST_6H_AGO" "$EMPTY_LEDGER")"
is "burst 6h ago: SP_SELF_REPEATING_N is 0" "SP_SELF_REPEATING_N=0" \
   "$(grep '^SP_SELF_REPEATING_N=' <<< "$out_6h")"
nowant "burst 6h ago: no SP_SELF_REPEATING0 key" "SP_SELF_REPEATING0=" "$out_6h"

# ======================================================================================
echo
echo "ACCEPTANCE 2: three consecutive in last 10 min → SP_SELF_REPEATING_N=1"

# The fixture has three consecutive passes each with the same ACT. The last pass is the
# most recent, so the run extends to the end of the window — it is "repeating now".
THREE_CONSEC="
${TS_10M} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_10M} spira: ACT handled 1 stranded item(s)
${TS_8M} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_8M} spira: ACT handled 1 stranded item(s)
${TS_6M} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_6M} spira: ACT handled 1 stranded item(s)
"
out_3="$(run_metrics "$THREE_CONSEC" "$EMPTY_LEDGER")"
is "three consecutive: SP_SELF_REPEATING_N is 1" "SP_SELF_REPEATING_N=1" \
   "$(grep '^SP_SELF_REPEATING_N=' <<< "$out_3")"
want "three consecutive: SP_SELF_REPEATING0 contains count" "3 passes" \
     "$(grep '^SP_SELF_REPEATING0=' <<< "$out_3")"
want "three consecutive: SP_SELF_REPEATING0 contains act text" "handled 1 stranded" \
     "$(grep '^SP_SELF_REPEATING0=' <<< "$out_3")"

# Verify a stopped repetition does NOT show: add a clean pass after the burst.
THREE_THEN_CLEAN="
${TS_10M} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_10M} spira: ACT handled 1 stranded item(s)
${TS_8M} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_8M} spira: ACT handled 1 stranded item(s)
${TS_6M} spira: state: goal=sp-foo open=1 plan_ready=1 in_progress=0
${TS_6M} spira: ACT handled 1 stranded item(s)
${TS_2M} spira: state: goal=sp-foo open=0 plan_ready=0 in_progress=0
"
out_stopped="$(run_metrics "$THREE_THEN_CLEAN" "$EMPTY_LEDGER")"
is "stopped burst: SP_SELF_REPEATING_N is 0 after clean pass" "SP_SELF_REPEATING_N=0" \
   "$(grep '^SP_SELF_REPEATING_N=' <<< "$out_stopped")"

# ======================================================================================
echo
echo "ACCEPTANCE 3: stillborn in-window renders alert; out-of-window does not"

# A 'born' with no 'awake' within the window is stillborn. Positive control first.
LEDGER_BORN_IN="
${TS_10M} born aeon-builder-sp-foo pid=1234
"
out_lb="$(run_metrics "" "$LEDGER_BORN_IN")"
is "stillborn in-window: SP_SELF_STILLBORN_W is 1" "SP_SELF_STILLBORN_W=1" \
   "$(grep '^SP_SELF_STILLBORN_W=' <<< "$out_lb")"
want "stillborn in-window: LAST is set to a time" "ago" \
     "$(grep '^SP_SELF_STILLBORN_LAST=' <<< "$out_lb")"

# An 'awake' that follows 'born' means it did NOT die at birth — should not count.
LEDGER_BORN_AWAKE="
${TS_10M} born aeon-builder-sp-foo pid=1234
${TS_10M} awake aeon-builder-sp-foo work
"
out_live="$(run_metrics "" "$LEDGER_BORN_AWAKE")"
is "born+awake: SP_SELF_STILLBORN_W is 0" "SP_SELF_STILLBORN_W=0" \
   "$(grep '^SP_SELF_STILLBORN_W=' <<< "$out_live")"

# Out-of-window: the 'born' happened 6h ago, outside the 90-min window.
LEDGER_BORN_OUT="
${TS_6H} born aeon-builder-sp-bar pid=5678
"
out_out="$(run_metrics "" "$LEDGER_BORN_OUT")"
is "stillborn out-of-window: SP_SELF_STILLBORN_W is 0" "SP_SELF_STILLBORN_W=0" \
   "$(grep '^SP_SELF_STILLBORN_W=' <<< "$out_out")"

# health.sh rendering the SELF section (REPEATING/BIRTH/STALL never appearing, whatever
# the flags say, because that section was removed) is one row in test-cockpit-probe-fault.sh's
# renderer table now (cluster 8, UC-16/21, docs/test-plan/cockpit-observability.md): the four
# fixtures here (none tripped, repeating only, stillborn only, starved only) all asserted the
# identical "still absent" property, so one fixture with every flag tripped at once proves it
# in a single row instead of four.

# ======================================================================================
echo
echo "health.sh: SP_PASS_SECS rendered in header"

RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"
COCKPIT_DIR="$(dirname "$HEALTH")"

run_health() {   # run_health: sources cockpit.env and renders once at 80 cols
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 TERM=dumb \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        bash "$HEALTH" once 0 80 2>/dev/null
}

# SP_PASS_SECS present → 'pass Ns' appears in header
cat > "$RUN/cockpit.env" <<'SNAP'
SP_AT='1000000000'
SP_PASS_SECS='247'
SP_SENTINEL_TIMER='1'
SP_SENTINEL_AGE='10'
SP_OPS_TIMER='1'
SP_OPS_AGE='10'
SP_SINCE_JUDGEMENT='3'
SP_SELF_REPEATING_N='0'
SP_SELF_STILLBORN_W='0'
SP_SELF_STILLBORN_LAST='-'
SP_SELF_STARVED_W='0'
SP_SELF_STARVED_LAST='-'
SNAP
h_pass="$(run_health)"
want   "pass_secs: 'pass 247s' in header"   "pass 247s"  "$h_pass"

# SP_PASS_SECS absent → 'pass' does not appear at all (key is optional)
cat > "$RUN/cockpit.env" <<'SNAP'
SP_AT='1000000000'
SP_SENTINEL_TIMER='1'
SP_SENTINEL_AGE='10'
SP_OPS_TIMER='1'
SP_OPS_AGE='10'
SP_SINCE_JUDGEMENT='3'
SP_SELF_REPEATING_N='0'
SP_SELF_STILLBORN_W='0'
SP_SELF_STILLBORN_LAST='-'
SP_SELF_STARVED_W='0'
SP_SELF_STARVED_LAST='-'
SNAP
h_nopass="$(run_health)"
nowant "no pass_secs: 'pass ' not in header" "pass " "$h_nopass"

# ======================================================================================
echo
printf 'test-cockpit-self: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
