#!/usr/bin/env bash
#
# test-watchtower-queue.sh — queue stall detectors in czar.sh --pass
#
# WHAT THIS SUITE IS FOR
# ----------------------
# czar.sh --pass is the deterministic queue-stall detector that runs every 30 s on its
# own timer. It reads landing.log for log-pattern-based detectors (DEADLOCK,
# ATTRIBUTION-FAILED, SORT-FAILED, LOOP-STALLED) and reads runtime state for
# resource-based detectors (CI-STALLED, STARVED). Each detector files one incident per
# class on first occurrence via incident.sh (deduped by SPIRA_INCIDENT_REF).
# All detectors are verified with a positive control before any absence assertion is
# believed (law-absence-needs-a-positive-control).
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
# covers: spira/czar.sh spira/sentinel.sh spira/watchtower.sh
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

# Stub systemctl: is-failed returns non-zero → loop-stalled takes inference branch.
printf '#!/usr/bin/env bash\nexit 1\n' > "$TMP/stub-sc.sh"
chmod +x "$TMP/stub-sc.sh"

# SPIRA_DB stub directory (lib.sh may reference it; incident.sh is mocked so no real db needed).
mkdir -p "$TMP/db"

fresh() {
    rm -rf "$TMP/run"
    mkdir -p "$TMP/run"
    rm -f "$TMP/inc-subjects" "$TMP/inc-refs"
}

# Append a log line with the given timestamp and text to the landing log.
log_line() {   # log_line <ts> <text>
    printf '%s spira: %s\n' "$1" "$2" >> "$TMP/run/landing.log"
}

# Run czar.sh --pass in a clean environment with all classes in act mode.
# Incident subjects are appended to $TMP/inc-subjects.
wt_qc() {   # wt_qc [VAR=val ...]
    local mock="$TMP/mock-inc.sh"
    printf '#!/usr/bin/env bash\nprintf "%%s\n" "$2" >> "%s"\ncat > /dev/null\n' \
        "$TMP/inc-subjects" > "$mock"
    chmod +x "$mock"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_DB="$TMP/db" \
        SPIRA_QUEUE_LOG="$TMP/run/landing.log" \
        SPIRA_CZAR_PASS_MARKER="$TMP/run/czar-pass.swept" \
        SPIRA_INCIDENT_SH="$mock" \
        SPIRA_SYSTEMCTL="$TMP/stub-sc.sh" \
        SPIRA_CZAR_STAGE_DEADLOCK=act \
        SPIRA_CZAR_STAGE_ATTRIBUTION_FAILED=act \
        SPIRA_CZAR_STAGE_SORT_FAILED=act \
        SPIRA_CZAR_STAGE_LOOP_STALLED=act \
        SPIRA_CZAR_STAGE_CI_STALLED=act \
        SPIRA_CZAR_STAGE_STARVED=act \
        SPIRA_CZAR_STAGE_CI_RED=act \
        "$@" bash "$HERE/czar.sh" --pass 2>/dev/null
}

# Run czar.sh --pass capturing SPIRA_INCIDENT_REF values to $TMP/inc-refs.
wt_qc_refs() {   # wt_qc_refs [VAR=val ...]
    local mock="$TMP/mock-inc-refs.sh"
    printf '#!/usr/bin/env bash\nprintf "%%s\n" "${SPIRA_INCIDENT_REF:-}" >> "%s"\ncat > /dev/null\n' \
        "$TMP/inc-refs" > "$mock"
    chmod +x "$mock"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_DB="$TMP/db" \
        SPIRA_QUEUE_LOG="$TMP/run/landing.log" \
        SPIRA_CZAR_PASS_MARKER="$TMP/run/czar-pass.swept" \
        SPIRA_INCIDENT_SH="$mock" \
        SPIRA_SYSTEMCTL="$TMP/stub-sc.sh" \
        SPIRA_CZAR_STAGE_DEADLOCK=act \
        SPIRA_CZAR_STAGE_ATTRIBUTION_FAILED=act \
        SPIRA_CZAR_STAGE_SORT_FAILED=act \
        SPIRA_CZAR_STAGE_LOOP_STALLED=act \
        SPIRA_CZAR_STAGE_CI_STALLED=act \
        SPIRA_CZAR_STAGE_STARVED=act \
        SPIRA_CZAR_STAGE_CI_RED=act \
        "$@" bash "$HERE/czar.sh" --pass 2>/dev/null
}

# ======================================================================================
echo
echo "positive controls — each detector must fire on its fixture:"
# ======================================================================================
# THE FIRST THING THIS SUITE PROVES IS REACHABILITY. Every absence assertion below
# could trivially pass if the whole --pass branch were a no-op; these controls
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
    > "$TMP/run/czar-pass.swept"
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
rm -f "$TMP/run/czar-pass.swept"
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
rm -f "$TMP/run/czar-pass.swept"
log_line "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "verdict spira: PR 73 red — no suites identified; leaving batch open"
wt_qc_refs
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
unique="$(printf '%s\n' "$refs" | sort -u | grep -c . 2>/dev/null || echo 0)"
is   "deadlock: two separate occurrences produce the same ref" "1" "$unique"
want "deadlock ref names the class" "queue-deadlock-batch-open" "$refs"

# Helper for CI-STALLED and STARVED tests that need extra seams.
# Sets up a mock forge script returning batch-ci-status output with the given queued-since epoch.
mock_forge() {   # mock_forge <epoch-or-empty>
    local epoch="$1"
    if [ -n "$epoch" ]; then
        printf '#!/usr/bin/env bash\ncase "$1" in batch-ci-status) printf "queued-since: %%s\n" "%s" ;; esac\n' \
            "$epoch" > "$TMP/mock-forge.sh"
    else
        printf '#!/usr/bin/env bash\n: # batch-ci-status returns nothing\n' > "$TMP/mock-forge.sh"
    fi
    chmod +x "$TMP/mock-forge.sh"
}

# Runs czar.sh --pass with CI-STALLED and STARVED seams applied.
wt_qc_ext() {   # wt_qc_ext [VAR=val ...]
    local mock="$TMP/mock-inc.sh"
    printf '#!/usr/bin/env bash\nprintf "%%s\n" "$2" >> "%s"\ncat > /dev/null\n' \
        "$TMP/inc-subjects" > "$mock"
    chmod +x "$mock"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_DB="$TMP/db" \
        SPIRA_QUEUE_LOG="$TMP/run/landing.log" \
        SPIRA_CZAR_PASS_MARKER="$TMP/run/czar-pass.swept" \
        SPIRA_INCIDENT_SH="$mock" \
        SPIRA_SYSTEMCTL="$TMP/stub-sc.sh" \
        SPIRA_REPO_MAP="$TMP/repo-map" \
        SPIRA_FORGE="$TMP/mock-forge.sh" \
        SPIRA_CZAR_STAGE_DEADLOCK=act \
        SPIRA_CZAR_STAGE_ATTRIBUTION_FAILED=act \
        SPIRA_CZAR_STAGE_SORT_FAILED=act \
        SPIRA_CZAR_STAGE_LOOP_STALLED=act \
        SPIRA_CZAR_STAGE_CI_STALLED=act \
        SPIRA_CZAR_STAGE_STARVED=act \
        SPIRA_CZAR_STAGE_CI_RED=act \
        "$@" bash "$HERE/czar.sh" --pass 2>/dev/null
}

# Set up the fake repo-map once (used by CI-STALLED tests via repo_root).
mkdir -p "$TMP/repo"
printf 'spira | %s | refs/heads/main | refs/heads/main\n' "$TMP/repo" > "$TMP/repo-map"

# ======================================================================================
echo
echo "positive control — CI-STALLED must fire on its fixture:"
# ======================================================================================
# THE FIXTURE FAILS AGAINST THE PREVIOUS CODE (no czar.sh CI-STALLED detector);
# after adding the detector, it fires exactly once per open batch repo.

fresh
mkdir -p "$TMP/run/queue/spira"
printf 'pr=99\nhead=abc123\nmembers=sp-test1:abc\nbranch=spira/queue/sp-batch-1\n' \
    > "$TMP/run/queue/spira/open"
mock_forge "$(( NOW - 700 ))"   # 700s in the past, threshold 600s
wt_qc_ext SPIRA_CI_QUEUED_MAX_SECS=600
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "CI-STALLED: fires when job queued 700s (threshold 600s)" "QUEUE: CI job queued" "$subjects"
want "CI-STALLED: subject names the repo" "spira" "$subjects"

# ======================================================================================
echo
echo "CI-STALLED — job queued with no runner:"
# ======================================================================================

# No open batch: no incident
fresh
mock_forge "$(( NOW - 700 ))"
wt_qc_ext SPIRA_CI_QUEUED_MAX_SECS=600
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "CI-STALLED absent when no open batch" "QUEUE: CI job queued" "$subjects"

# Queued time below threshold: no incident
fresh
mkdir -p "$TMP/run/queue/spira"
printf 'pr=99\nhead=abc123\nmembers=sp-test1:abc\nbranch=spira/queue/sp-batch-1\n' \
    > "$TMP/run/queue/spira/open"
mock_forge "$(( NOW - 100 ))"   # 100s in the past, threshold 600s
wt_qc_ext SPIRA_CI_QUEUED_MAX_SECS=600
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "CI-STALLED absent when queued only 100s (threshold 600s)" "QUEUE: CI job queued" "$subjects"

# No queued jobs (forge returns empty): no incident
fresh
mkdir -p "$TMP/run/queue/spira"
printf 'pr=99\nhead=abc123\nmembers=sp-test1:abc\nbranch=spira/queue/sp-batch-1\n' \
    > "$TMP/run/queue/spira/open"
mock_forge ""   # no queued jobs
wt_qc_ext SPIRA_CI_QUEUED_MAX_SECS=600
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "CI-STALLED absent when forge returns empty (no queued jobs)" "QUEUE: CI job queued" "$subjects"

# Open batch with no branch= field: no incident (malformed open file)
fresh
mkdir -p "$TMP/run/queue/spira"
printf 'pr=99\nhead=abc123\nmembers=sp-test1:abc\n' \
    > "$TMP/run/queue/spira/open"
mock_forge "$(( NOW - 700 ))"
wt_qc_ext SPIRA_CI_QUEUED_MAX_SECS=600
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "CI-STALLED absent when open file has no branch= field" "QUEUE: CI job queued" "$subjects"

# CI-STALLED ref is stable across runs
fresh
mkdir -p "$TMP/run/queue/spira"
printf 'pr=99\nhead=abc123\nmembers=sp-test1:abc\nbranch=spira/queue/sp-batch-1\n' \
    > "$TMP/run/queue/spira/open"
mock_forge "$(( NOW - 700 ))"
rm -f "$TMP/inc-refs"
wt_qc_refs_ext() {
    local mock="$TMP/mock-inc-refs.sh"
    printf '#!/usr/bin/env bash\nprintf "%%s\n" "${SPIRA_INCIDENT_REF:-}" >> "%s"\ncat > /dev/null\n' \
        "$TMP/inc-refs" > "$mock"
    chmod +x "$mock"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_DB="$TMP/db" \
        SPIRA_QUEUE_LOG="$TMP/run/landing.log" \
        SPIRA_CZAR_PASS_MARKER="$TMP/run/czar-pass.swept" \
        SPIRA_INCIDENT_SH="$mock" \
        SPIRA_SYSTEMCTL="$TMP/stub-sc.sh" \
        SPIRA_REPO_MAP="$TMP/repo-map" \
        SPIRA_FORGE="$TMP/mock-forge.sh" \
        SPIRA_CZAR_STAGE_DEADLOCK=act \
        SPIRA_CZAR_STAGE_ATTRIBUTION_FAILED=act \
        SPIRA_CZAR_STAGE_SORT_FAILED=act \
        SPIRA_CZAR_STAGE_LOOP_STALLED=act \
        SPIRA_CZAR_STAGE_CI_STALLED=act \
        SPIRA_CZAR_STAGE_STARVED=act \
        SPIRA_CZAR_STAGE_CI_RED=act \
        "$@" bash "$HERE/czar.sh" --pass 2>/dev/null
}
wt_qc_refs_ext SPIRA_CI_QUEUED_MAX_SECS=600
rm -f "$TMP/run/czar-pass.swept"
wt_qc_refs_ext SPIRA_CI_QUEUED_MAX_SECS=600
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
unique="$(printf '%s\n' "$refs" | sort -u | grep -c . 2>/dev/null || echo 0)"
is   "ci-stalled: two runs produce the same ref" "1" "$unique"
want "ci-stalled ref names the class and repo"   "queue-ci-stalled-spira" "$refs"

# ======================================================================================
echo
echo "positive control — STARVED must fire on its fixture:"
# ======================================================================================
# THE FIXTURE FAILS AGAINST THE PREVIOUS CODE (no czar.sh STARVED detector).

fresh
STARVED_FIRST_OLD="$(( NOW - 1500 ))"   # 25 minutes, threshold 20m (1200s)
printf '{"plan,spira:starved:-":{"first":%s,"acted":0,"escalated":0}}\n' \
    "$STARVED_FIRST_OLD" > "$TMP/run/strands.json"
wt_qc SPIRA_STRANDS_STATE="$TMP/run/strands.json" SPIRA_STARVED_MAX_MINS=20
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "STARVED: fires when partition starved 25m (threshold 20m)" "QUEUE:" "$subjects"
want "STARVED: subject names the partition"                       "plan,spira" "$subjects"

# ======================================================================================
echo
echo "STARVED — ready work with no serving aeons:"
# ======================================================================================

# Below threshold: no incident
fresh
STARVED_FIRST_RECENT="$(( NOW - 300 ))"   # 5 minutes, threshold 20m
printf '{"plan,spira:starved:-":{"first":%s,"acted":0,"escalated":0}}\n' \
    "$STARVED_FIRST_RECENT" > "$TMP/run/strands.json"
wt_qc SPIRA_STRANDS_STATE="$TMP/run/strands.json" SPIRA_STARVED_MAX_MINS=20
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "STARVED absent when starved only 5m (threshold 20m)" "QUEUE:" "$subjects"

# Wrong kind (ghost, not starved): no incident
fresh
printf '{"plan,spira:ghost:sp-123":{"first":%s,"acted":0,"escalated":0}}\n' \
    "$STARVED_FIRST_OLD" > "$TMP/run/strands.json"
wt_qc SPIRA_STRANDS_STATE="$TMP/run/strands.json" SPIRA_STARVED_MAX_MINS=20
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "STARVED absent when only ghost entries (not starved)" "QUEUE:" "$subjects"

# No strands.json: no incident
fresh
wt_qc SPIRA_STRANDS_STATE="$TMP/run/strands.json" SPIRA_STARVED_MAX_MINS=20
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "STARVED absent when strands.json does not exist" "QUEUE:" "$subjects"

# STARVED ref is stable across runs with different measured ages
fresh
printf '{"plan,spira:starved:-":{"first":%s,"acted":0,"escalated":0}}\n' \
    "$STARVED_FIRST_OLD" > "$TMP/run/strands.json"
rm -f "$TMP/inc-refs"
wt_qc_refs SPIRA_STRANDS_STATE="$TMP/run/strands.json" SPIRA_STARVED_MAX_MINS=20
rm -f "$TMP/run/czar-pass.swept"
wt_qc_refs SPIRA_STRANDS_STATE="$TMP/run/strands.json" SPIRA_STARVED_MAX_MINS=20
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
unique="$(printf '%s\n' "$refs" | sort -u | grep -c . 2>/dev/null || echo 0)"
is   "starved: two runs produce the same ref" "1" "$unique"
want "starved ref names the class and partition" "queue-starved-plan-spira" "$refs"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
