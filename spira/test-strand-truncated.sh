#!/usr/bin/env bash
#
# test-strand-truncated.sh — a sentinel pass truncated before evaluating a partition is
#   reported as 'pass-truncated' (info) by strand.sh, not as 'starved' (escalate).
#
#   ./test-strand-truncated.sh
#
# WHY THIS EXISTS. sp-ow8n: when bd calls are slow under lock contention, a sentinel pass
# may not reach every declared partition within its allotted time. The partitions it skips
# are absent from the log — identical to "nothing ready" from strand.sh's perspective.
# strand.sh then reports 'starved' and points at the sentinel timer, which is healthy.
#
# The fix is two parts:
#   sentinel.sh:  log "CHECK7 $f: not evaluated (pass budget exhausted)" when the budget
#                 runs out before a fayth is evaluated.
#   strand.sh:    detect that line in the most recent pass and emit 'pass-truncated' (info)
#                 instead of 'starved' (escalate).
#
# FOUR CASES (law-absence-needs-a-positive-control):
#
#   0. POSITIVE CONTROL (classify.py) — PASS_TRUNCATED=0, ready beads, no live aeon →
#      classifier DOES produce "starved". Proves the check can fire before trusting silence.
#
#   1. PASS TRUNCATED (classify.py) — PASS_TRUNCATED=1, same fixture → "pass-truncated"
#      IS emitted as an info row; "starved" IS NOT emitted.
#
#   2. STRAND.SH DETECTION — sentinel log contains "not evaluated" for a fayth working
#      this partition → strand.sh report emits "pass-truncated", not "starved".
#
#   3. NO TRUNCATION IN LOG — sentinel log exists but has no "not evaluated" line →
#      strand.sh report emits "starved" as before.
#
#   4. SENTINEL BUDGET — with SPIRA_SENTINEL_PASS_BUDGET_SECS=0 and two task fayths,
#      sentinel.sh logs "not evaluated (pass budget exhausted)" for both.
#
# defect: sp-ow8n
# covers: spira/strand-classify.py spira/strand.sh spira/sentinel.sh spira/lib.sh
# hermetic-ok: cases 0-3 use no database; case 4 uses no database (SPIRA_SKIP_RECLAIM=1)
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run"

# One ready bead, no live aeons — the fixture that starved fires on.
BEADS='[{"id":"sp-t1","title":"test bead","status":"open","labels":["spira","plan"]}]'
READY='[{"id":"sp-t1","title":"test bead","status":"open","labels":["spira","plan"]}]'

classify_py() {
    local truncated="${1:-0}"
    printf '%s' "$BEADS" > "$TMP/beads.json"
    printf '%s' "$READY"  > "$TMP/ready.json"
    BEADS_FILE="$TMP/beads.json" \
    READY_FILE="$TMP/ready.json" \
    HOLDERS="" LIVE=0 GHOST_GRACE=300 \
    SPIRA_ASK_LABEL=needs-operator \
    CAPACITY_PAUSED=0 \
    PASS_TRUNCATED="$truncated" \
        python3 "$HERE/strand-classify.py"
}

echo "test-strand-truncated.sh"

# ======================================================================================
echo
echo "case 0 — positive control: PASS_TRUNCATED=0 → classifier DOES produce starved:"
# ======================================================================================
out="$(classify_py 0)"
want "positive control: starved IS raised" "starved" "$out"

# ======================================================================================
echo
echo "case 1 — pass truncated: PASS_TRUNCATED=1 → pass-truncated info, no starved:"
# ======================================================================================
out="$(classify_py 1)"
want   "pass-truncated IS emitted"    "pass-truncated" "$out"
nowant "starved IS NOT emitted"       "starved"        "$out"
want   "disposition is info"          "info"           "$out"

# ======================================================================================
echo
echo "case 2 — strand.sh detection: sentinel log 'not evaluated' → pass-truncated:"
# ======================================================================================
# Build a stub chamber with a fayth whose partition is "spira,test-plan". The sentinel
# log records that this fayth was not evaluated. Strand.sh detects it and passes
# PASS_TRUNCATED=1 to the classifier.
mkdir -p "$TMP/stubs/chamber"
cat > "$TMP/stubs/chamber/test-watcher.fayth" <<'FAYTH'
FAYTH_NAME=test-watcher
FAYTH_LABELS=spira,test-plan
FAYTH_EXCLUDE_LABELS=spira-poison,needs-operator
FAYTH_MAX_CONCURRENT=1
FAYTH_TIMEOUT_SECONDS=3600
FAYTH

# Mock bd returns the fixture bead for any list/ready query.
# bdq() prepends -C <db> to every call; strip it before matching the subcommand.
cat > "$TMP/mock-bd" <<'MOCKBD'
#!/usr/bin/env bash
case "${1:-}" in -C) shift 2 ;; esac
case "${1:-}" in
    list)  printf '[{"id":"sp-t1","title":"test bead","status":"open","labels":["spira","test-plan"]}]\n' ;;
    ready) printf '[{"id":"sp-t1","title":"test bead","status":"open","labels":["spira","test-plan"]}]\n' ;;
    *)     exit 0 ;;
esac
MOCKBD
chmod +x "$TMP/mock-bd"

# Sentinel log: one completed pass (state: goal= line) followed by a "not evaluated"
# line for test-watcher in that same pass.
cat > "$TMP/run/sentinel.log" <<'LOG'
2026-01-01T00:00:00Z spira: state: goal=sp-goal open=1 plan_ready=1 in_progress=0 aeons=0
2026-01-01T00:00:01Z spira: CHECK7 test-watcher: not evaluated (pass budget exhausted)
LOG

out="$(
    SPIRA_HOME="$TMP/stubs" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_BD="$TMP/mock-bd" \
    SPIRA_SUMMON=stub \
    SPIRA_LABELS="spira,test-plan" \
        bash "$HERE/strand.sh" report 2>/dev/null
)"
want   "detection: pass-truncated IS reported" "pass-truncated" "$out"
nowant "detection: starved IS NOT reported"    "starved"        "$out"

# ======================================================================================
echo
echo "case 3 — no truncation in log: strand.sh still reports starved:"
# ======================================================================================
# Same setup but sentinel log has no "not evaluated" line for test-watcher.
cat > "$TMP/run/sentinel.log" <<'LOG'
2026-01-01T00:00:00Z spira: state: goal=sp-goal open=1 plan_ready=1 in_progress=0 aeons=0
2026-01-01T00:00:01Z spira: CHECK7 test-watcher: nothing ready in its partition
LOG

out="$(
    SPIRA_HOME="$TMP/stubs" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_BD="$TMP/mock-bd" \
    SPIRA_SUMMON=stub \
    SPIRA_LABELS="spira,test-plan" \
        bash "$HERE/strand.sh" report 2>/dev/null
)"
want   "no truncation: starved IS raised"         "starved"        "$out"
nowant "no truncation: pass-truncated not raised" "pass-truncated" "$out"

# ======================================================================================
echo
echo "case 4 — sentinel budget: SPIRA_SENTINEL_PASS_BUDGET_SECS=0 logs both fayths as not evaluated:"
# ======================================================================================
# Sentinel runs with two task fayths and a zero budget. Both fayths should be logged
# as "not evaluated (pass budget exhausted)" without calling summon_fayth for either.
mkdir -p "$TMP/stubs2/chamber"
for _f in alpha beta; do
    cat > "$TMP/stubs2/chamber/$_f.fayth" <<FAYTH2
FAYTH_NAME=$_f
FAYTH_LABELS=spira,test-$_f
FAYTH_EXCLUDE_LABELS=spira-poison,needs-operator
FAYTH_MAX_CONCURRENT=1
FAYTH_TIMEOUT_SECONDS=3600
FAYTH2
done
# Stubs: pilgrimage, strand, sending, governor, reflect all exit 0 with no output.
for _s in pilgrimage.sh strand.sh sending.sh governor.sh reflect.sh; do
    printf '#!/bin/sh\n' > "$TMP/stubs2/$_s"; chmod +x "$TMP/stubs2/$_s"
done
# systemctl stub: says unit is inactive so the landing dispatch path runs cleanly.
printf '#!/bin/sh\necho inactive\n' > "$TMP/stubs2/mock-systemctl"; chmod +x "$TMP/stubs2/mock-systemctl"
# launch/summon/notify stubs.
for _s in mock-launch mock-summon mock-notify; do
    printf '#!/bin/sh\nexit 0\n' > "$TMP/stubs2/$_s"; chmod +x "$TMP/stubs2/$_s"
done
touch "$TMP/stubs2/repo-map"

_run2="$TMP/run2"; mkdir -p "$_run2"
out="$(env -i \
    PATH="$PATH" HOME="$HOME" \
    SPIRA_HOME="$TMP/stubs2" \
    SPIRA_RUN="$_run2" \
    SPIRA_DB="$TMP/no-db" \
    SPIRA_BD="$TMP/mock-bd" \
    SPIRA_GOAL="sp-goal" \
    SPIRA_FAYTHS="alpha beta" \
    SPIRA_SKIP_RECLAIM=1 \
    SPIRA_SKIP_CLOSED_CHECK=1 \
    SPIRA_SENTINEL_PASS_BUDGET_SECS=0 \
    SPIRA_LAND_STALE=999999 \
    SPIRA_SYSTEMCTL="$TMP/stubs2/mock-systemctl" \
    SPIRA_LAUNCH="$TMP/stubs2/mock-launch" \
    SPIRA_SUMMON="$TMP/stubs2/mock-summon" \
    SPIRA_NOTIFY="$TMP/stubs2/mock-notify" \
        bash "$HERE/sentinel.sh" 2>&1)"

want   "alpha logged as not evaluated" "CHECK7 alpha: not evaluated (pass budget exhausted)" "$out"
want   "beta logged as not evaluated"  "CHECK7 beta: not evaluated (pass budget exhausted)"  "$out"
nowant "alpha NOT logged as summoned"  "summoned a alpha"                                    "$out"
nowant "beta NOT logged as summoned"   "summoned a beta"                                     "$out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
