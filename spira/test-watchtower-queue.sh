#!/usr/bin/env bash
#
# test-watchtower-queue.sh — queue stall detectors in watchtower --queue-checks
#
# WHAT THIS SUITE IS FOR
# ----------------------
# watchtower --queue-checks is the landing-cadence detector that reads landing.log
# for patterns that block the merge queue and files one incident per class on first
# occurrence. Each detector is verified with a positive control before any absence
# assertion is believed (law-absence-needs-a-positive-control).
#
# THE FIXTURE LANDING.LOG IS WRITTEN BY HAND. The log format is plain text
# (ISO timestamp + prose), and the detectors are pure string matches. A hand-written
# fixture is correct here; there is no multi-field binary format whose seam matters.
#
# EACH INCIDENT REF IS STABLE ACROSS PASSES. Two calls with the same condition but
# different measured values (e.g. stall duration changes each pass) must produce the
# same SPIRA_INCIDENT_REF so incident.sh dedup bumps a recurrence rather than filing
# a new bead on every sentinel tick (law-alerts-must-be-actionable).
#
# covers: spira/watchtower.sh spira/sentinel.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-watchtower-queue.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
NOW="$(date +%s)"
NOW_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
PAST_TS="$(date -u -d "@$(( NOW - 7200 ))" +%Y-%m-%dT%H:%M:%SZ)"

fresh() {
    rm -rf "$TMP/run"
    mkdir -p "$TMP/run"
    rm -f "$TMP/inc-subjects" "$TMP/inc-refs"
}

# Append a log line with the given timestamp and text to the landing log.
log_line() {   # log_line <ts> <text>
    printf '%s spira: %s\n' "$1" "$2" >> "$TMP/run/landing.log"
}

# Run watchtower --queue-checks in a clean environment.
# Incident subjects are appended to $TMP/inc-subjects.
wt_qc() {   # wt_qc [VAR=val ...]
    local mock="$TMP/mock-inc.sh"
    printf '#!/usr/bin/env bash\nprintf "%%s\n" "$2" >> "%s"\ncat > /dev/null\n' \
        "$TMP/inc-subjects" > "$mock"
    chmod +x "$mock"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_QUEUE_LOG="$TMP/run/landing.log" \
        SPIRA_QUEUE_CHECK_MARKER="$TMP/run/queue-check.swept" \
        SPIRA_INCIDENT_SH="$mock" \
        "$@" bash "$HERE/watchtower.sh" --queue-checks 2>/dev/null
}

# Run watchtower --queue-checks capturing SPIRA_INCIDENT_REF values to $TMP/inc-refs.
wt_qc_refs() {   # wt_qc_refs [VAR=val ...]
    local mock="$TMP/mock-inc-refs.sh"
    printf '#!/usr/bin/env bash\nprintf "%%s\n" "${SPIRA_INCIDENT_REF:-}" >> "%s"\ncat > /dev/null\n' \
        "$TMP/inc-refs" > "$mock"
    chmod +x "$mock"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_QUEUE_LOG="$TMP/run/landing.log" \
        SPIRA_QUEUE_CHECK_MARKER="$TMP/run/queue-check.swept" \
        SPIRA_INCIDENT_SH="$mock" \
        "$@" bash "$HERE/watchtower.sh" --queue-checks 2>/dev/null
}

# ======================================================================================
echo
echo "positive controls — each detector must fire on its fixture:"
# ======================================================================================
# THE FIRST THING THIS SUITE PROVES IS REACHABILITY. Every absence assertion below
# could trivially pass if the whole --queue-checks branch were a no-op; these controls
# plant the exact fixture each detector is meant to find and require it to fire.

# DEADLOCK
fresh
log_line "$NOW_TS" "verdict spira: PR 72 red — no suites identified; leaving batch open"
wt_qc
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "DEADLOCK: fires on its log fixture" "QUEUE: batch open" "$subjects"
want "DEADLOCK: subject names the class"  "DEADLOCK"          "$subjects"

# ATTRIBUTION-FAILED
fresh
log_line "$NOW_TS" "verdict spira: PR 73 — ejected 0, requeued 5"
wt_qc
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "ATTRIBUTION-FAILED: fires on ejected 0, requeued 5" "QUEUE: attribution ejected 0" "$subjects"

# SORT-FAILED
fresh
log_line "$NOW_TS" "queue_sort_rows: ranking failed (rc=1) -- returning rows unranked"
wt_qc
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "SORT-FAILED: fires on ranking failed fixture" "QUEUE: queue_sort_rows" "$subjects"

# LOOP-STALLED
fresh
OLD_PASS_TS="$(date -u -d "@$(( NOW - 4000 ))" +%Y-%m-%dT%H:%M:%SZ)"
log_line "$OLD_PASS_TS" "landing: pass complete — 3 branch(es) seen, 0 movement(s)"
wt_qc SPIRA_LOOP_STALL_SECS=3000
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "LOOP-STALLED: fires when last pass is 4000s ago (threshold 3000s)" \
     "QUEUE: landing loop stalled" "$subjects"

# ======================================================================================
echo
echo "DEADLOCK — no suites identified; leaving batch open:"
# ======================================================================================

# No such pattern: no incident
fresh
log_line "$NOW_TS" "verdict spira: PR 72 red — suites identified; failing attribution"
wt_qc
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "no 'no suites' line → no deadlock incident" "QUEUE: batch open" "$subjects"

# Pattern before marker: filtered out
fresh
log_line "$PAST_TS" "verdict spira: PR 72 red — no suites identified; leaving batch open"
printf '%s\n' "$(date -u -d "@$(( NOW - 3600 ))" +%Y-%m-%dT%H:%M:%SZ)" \
    > "$TMP/run/queue-check.swept"
wt_qc
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "line before marker not re-detected" "QUEUE: batch open" "$subjects"

# ======================================================================================
echo
echo "ATTRIBUTION-FAILED — ejected 0, requeued N:"
# ======================================================================================

# Ejected non-zero: attribution isolated a survivor — not a whole-batch requeue
fresh
log_line "$NOW_TS" "verdict spira: PR 73 — ejected 2, requeued 1"
wt_qc
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "ejected non-zero: not attribution-failed" "QUEUE: attribution" "$subjects"

# Ejected 0, requeued 0: nothing queued, no-op
fresh
log_line "$NOW_TS" "verdict spira: PR 73 — ejected 0, requeued 0"
wt_qc
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "requeued 0: not attribution-failed" "QUEUE: attribution" "$subjects"

# Multi-digit requeued count: still fires (pattern not anchored at digit boundary)
fresh
log_line "$NOW_TS" "verdict spira: PR 74 — ejected 0, requeued 10"
wt_qc
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "ejected 0, requeued 10 fires attribution-failed" "QUEUE: attribution" "$subjects"

# ======================================================================================
echo
echo "LOOP-STALLED — no landing pass complete within the threshold:"
# ======================================================================================

# Recent pass: no incident
fresh
RECENT_TS="$(date -u -d "@$(( NOW - 100 ))" +%Y-%m-%dT%H:%M:%SZ)"
log_line "$RECENT_TS" "landing: pass complete — 3 branch(es) seen, 0 movement(s)"
wt_qc SPIRA_LOOP_STALL_SECS=3000
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "100s-old pass below 3000s threshold: no incident" "QUEUE: landing loop stalled" "$subjects"

# No landing.log: cannot tell stalled vs. never started — no incident
fresh
wt_qc SPIRA_LOOP_STALL_SECS=3000
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "no log file: no incident" "QUEUE: landing loop stalled" "$subjects"

# Log exists but has no 'pass complete' line: no incident
fresh
log_line "$NOW_TS" "verdict spira: PR 72 red"
wt_qc SPIRA_LOOP_STALL_SECS=3000
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "log with no pass-complete: no incident" "QUEUE: landing loop stalled" "$subjects"

# Threshold is configurable: a 100s-old pass triggers at threshold=50s
fresh
PASS_TS="$(date -u -d "@$(( NOW - 100 ))" +%Y-%m-%dT%H:%M:%SZ)"
log_line "$PASS_TS" "landing: pass complete — 3 branch(es) seen, 0 movement(s)"
wt_qc SPIRA_LOOP_STALL_SECS=50
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "configurable threshold: 100s age fires at threshold=50s" \
     "QUEUE: landing loop stalled" "$subjects"

# ======================================================================================
echo
echo "marker advances after each run — same log line not re-fired:"
# ======================================================================================
# THE SEAM THIS COVERS. A detector that re-fires on every pass for the same line
# would flood incident.sh with calls. The marker records the timestamp of the last
# check; only lines after the marker are new. Verify that after a run, the same
# line does not trigger again.

fresh
log_line "$NOW_TS" "verdict spira: PR 72 red — no suites identified; leaving batch open"
wt_qc
subjects1="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "first run: line detected" "QUEUE: batch open" "$subjects1"

rm -f "$TMP/inc-subjects"
wt_qc
subjects2="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "second run: same line not re-fired" "QUEUE: batch open" "$subjects2"

# ======================================================================================
echo
echo "halted world skips all queue checks:"
# ======================================================================================
# A deliberately halted world has nothing running — the queue looks stalled not
# because of a bug but because of a deliberate stop. No incident must be filed.
# (law-a-deliberate-state-is-not-a-fault)

fresh
log_line "$NOW_TS" "verdict spira: PR 72 red — no suites identified; leaving batch open"
printf 'halted\n' > "$TMP/run/world.halted"
wt_qc
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "halted world files no incidents" "QUEUE:" "$subjects"

# Running world (no halt stamp) still fires: positive control for the halt guard
fresh
log_line "$NOW_TS" "verdict spira: PR 72 red — no suites identified; leaving batch open"
# No world.halted stamp.
wt_qc
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "running world still files the incident" "QUEUE: batch open" "$subjects"

# ======================================================================================
echo
echo "incident refs are stable across runs with different measured values:"
# ======================================================================================
# SPIRA_INCIDENT_REF must be the same on every call for the same condition so that
# incident.sh dedup bumps the recurrence count rather than filing a second bead.
# The LOOP-STALLED subject embeds the age in seconds; without a stable ref, each
# pass would produce a fresh bead rather than a recurrence count.

fresh
OLD_TS="$(date -u -d "@$(( NOW - 4000 ))" +%Y-%m-%dT%H:%M:%SZ)"
log_line "$OLD_TS" "landing: pass complete — 1 branch(es) seen, 0 movement(s)"
rm -f "$TMP/inc-refs"
wt_qc_refs SPIRA_LOOP_STALL_SECS=3000
rm -f "$TMP/run/queue-check.swept"
wt_qc_refs SPIRA_LOOP_STALL_SECS=3000
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
unique="$(printf '%s\n' "$refs" | sort -u | grep -c . 2>/dev/null || echo 0)"
is   "loop-stalled: two runs produce the same ref" "1" "$unique"
want "loop-stalled ref names the class"            "queue-loop-stalled" "$refs"

# DEADLOCK ref is also stable
fresh
rm -f "$TMP/inc-refs"
log_line "$NOW_TS" "verdict spira: PR 72 red — no suites identified; leaving batch open"
wt_qc_refs
rm -f "$TMP/run/queue-check.swept"
log_line "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "verdict spira: PR 73 red — no suites identified; leaving batch open"
wt_qc_refs
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
unique="$(printf '%s\n' "$refs" | sort -u | grep -c . 2>/dev/null || echo 0)"
is   "deadlock: two separate occurrences produce the same ref" "1" "$unique"
want "deadlock ref names the class" "queue-deadlock-batch-open" "$refs"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
