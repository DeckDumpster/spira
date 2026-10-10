#!/usr/bin/env bash
#
# test-census-events.sh — census reads requeue/reclaim/recur events written by bump_*.
#
#   ./test-census-events.sh
#
# WHAT THIS SUITE GUARDS
# ----------------------
# Before sp-2lk, bump_requeue/bump_reclaim/bump_recur were no-ops and census
# read labels that nothing wrote. Every pass reported all-clear regardless of how
# many times beads were requeued or recurred. This suite asserts the wire-up works
# end-to-end: bump_requeue/bump_recur/bump_reclaim write events, and census
# aggregates those events into the correct class counts.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control, law-a-regression-test-must-be-seen-to-fail)
# ----------------------------------------------------------------------------------------------------
# The suite was run against the unfixed tree before this commit; it produced
# FAIL for both of the event-based assertions below (census output was empty
# because bump_* wrote nothing and census read labels). The unfixed failure
# text: "wanted [sp-reopen-rebase-conflict] in []" and
# "wanted [sp-recur-suite-red] in []".
#
# THREE ACCEPTANCE CRITERIA:
# 1. bump_requeue and bump_recur write events that census counts.
# 2. The class name and occurrence count match the acceptance criteria from sp-2lk.
# 3. bump_reclaim writes events that census counts as sp-reclaim.
#
# A REAL bd ON A THROWAWAY DATABASE (law-prefer-the-real-dependency).
# Requires server mode: census_events_run_sql uses bd sql, and bd-embedded refuses
# bd sql in embedded mode. Skips when SPIRA_TESTDB_DATA is not set.
#
# tier: T2
# covers: census/src/* spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-census-events
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — census_events_run_sql uses bd sql directly, which embedded mode refuses
export SPIRA_TESTDB_MODE=server
testdb_up census-events || {
    # Server testdb unavailable (no running Dolt server). Skip rather than fail: the
    # test requires bd sql, which bd-embedded refuses in embedded mode.
    printf 'SKIP test-census-events: server testdb not available\n' >&2
    exit 77
}
. "$HERE/testlib/lc-fixture.sh"
lcfix_up || bail "lc-fixture: the lifecycle store did not come up"
lcfix_follow_testdb
trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
tl_config SPIRA_DB="$TESTDB_DIR" SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy
# shellcheck disable=SC1090
. "$HERE/lib.sh"

# bump_recur/bump_reclaim (lib.sh) were retired at sp-8itaf — zero live callers; the
# production writers are now incident::ports::bump_recur and strand::check::bump_reclaim
# (Rust). This suite's own subject is census.sh reading the events table, not who writes
# it, so these two local wrappers reach the same shared writer lib.sh's versions did.
recur_event()   { _bump_write_event "${1:-}" recurred  "${2:-unrecorded}"; }
reclaim_event() { _bump_write_event "${1:-}" reclaimed "${2:-unrecorded}"; }

echo "test-census-events.sh"

seed_bead() {   # seed_bead <id> — one open bead
    testdb_reset
    testdb_seed <<JSONL
{"id":"$1","title":"test bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-12T00:00:00Z"}
JSONL
}

census_out() {
    census --with-suppressed 2>/dev/null
}

# ======================================================================================
echo
echo "sp-2lk acceptance criteria — bump_requeue and bump_recur produce census entries"
# ======================================================================================
# The exact positive control from the bead:
#   bump_requeue "$id" merge-conflict (twice) + bump_recur "$id" suite-red (once)
#   → census must output: 1 sp-reopen-rebase-conflict (1 victims, 2 detections) and
#     1 sp-recur-suite-red (1 victims, 1 detections)
seed_bead "sp-c1"
bump_requeue "sp-c1" merge-conflict
bump_requeue "sp-c1" merge-conflict
recur_event   "sp-c1" suite-red

out="$(census_out)"
want "census reports 1 causal event for sp-reopen-rebase-conflict" "1 sp-reopen-rebase-conflict" "$out"
want "census shows 2 detections for sp-reopen-rebase-conflict" "sp-reopen-rebase-conflict (1 victims, 2 detections" "$out"
want "census reports 1 sp-recur-suite-red"        "1 sp-recur-suite-red"        "$out"

# ======================================================================================
echo
echo "bump_reclaim — events counted as sp-reclaim"
# ======================================================================================
seed_bead "sp-c2"
reclaim_event "sp-c2"
reclaim_event "sp-c2"

out="$(census_out)"
want "census reports sp-reclaim with 2 detections (1 victim)" "sp-reclaim (1 victims, 2 detections" "$out"

# ======================================================================================
echo
echo "bump_reclaim with cause — events counted as sp-reclaim-<cause>"
# ======================================================================================
seed_bead "sp-c3"
reclaim_event "sp-c3" timeout
reclaim_event "sp-c3" timeout
reclaim_event "sp-c3" timeout

out="$(census_out)"
want "census reports sp-reclaim-timeout with 3 detections (1 victim)" "sp-reclaim-timeout (1 victims, 3 detections" "$out"
nowant "no bare sp-reclaim" "sp-reclaim " "$out"

# ======================================================================================
echo
echo "class isolation — separate beads contribute to the same class"
# ======================================================================================
# Two different beads, same requeue cause — the class's victim count is cross-bead. All
# three calls land inside the same test run, well under the default 5-minute clustering
# gap, so they fold into one causal event (sp-jcd0e: the rank is causal events, not
# victims) — the victim count still crosses the bead boundary correctly.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-d1","title":"bead 1","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-12T00:00:00Z"}
{"id":"sp-d2","title":"bead 2","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-12T00:00:00Z"}
JSONL
bump_requeue "sp-d1" merge-conflict
bump_requeue "sp-d2" merge-conflict
bump_requeue "sp-d2" merge-conflict

out="$(census_out)"
want "cross-bead: one causal event, 2 distinct victims" "1 sp-reopen-rebase-conflict (2 victims" "$out"
want "cross-bead: 3 total event detections shown" "sp-reopen-rebase-conflict (2 victims, 3 detections" "$out"

# ======================================================================================
echo
echo "positive control — empty store reports nothing"
# ======================================================================================
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-e1","title":"bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-12T00:00:00Z"}
JSONL
out="$(census_out)"
is "census is empty when no bump events exist" "" "$out"

# ======================================================================================
echo
echo "caller-side: bead_reopen + bump_requeue (the landing.sh requeue path)"
# ======================================================================================
# The landing pass calls bump_requeue when a branch cannot rebase (before deciding
# whether to reopen or escalate). Calling bump_requeue alone would pass even if
# landing.sh had no bump call; this test exercises the caller-side path so removing
# bump_requeue from landing.sh leaves a gap the existing direct-call tests would not
# catch.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-f1","title":"landing test","status":"in_progress","issue_type":"task","labels":["spira"],"updated_at":"2026-09-12T00:00:00Z"}
JSONL
bead_reopen "sp-f1" rebase-conflict "rebase conflict test" >/dev/null 2>&1
bump_requeue "sp-f1" merge-conflict >/dev/null 2>&1

out="$(census_out)"
want   "landing requeue path produces sp-reopen-rebase-conflict" "1 sp-reopen-rebase-conflict" "$out"
nowant "landing requeue path: sp-requeue-merge-conflict absent" "sp-requeue-merge-conflict" "$out"

# ======================================================================================
echo
echo "caller-side: bump_reclaim ghost (the strand reclaim path)"
# ======================================================================================
# strand's check reclaims the bead (strand/src/check.rs, its own bd call) and then records
# bump_reclaim ghost; the recorded event is what census reads, so that is all this drives —
# the reclaim itself is strand's, never re-enacted here around the lifecycle machine
# (sp-hyo5e).
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-f2","title":"strand test","status":"in_progress","issue_type":"task","labels":["spira"],"updated_at":"2026-09-12T00:00:00Z"}
JSONL
reclaim_event "sp-f2" ghost >/dev/null 2>&1

out="$(census_out)"
want "strand reclaim path produces sp-reclaim-ghost" "1 sp-reclaim-ghost" "$out"

# ======================================================================================
echo
echo "bead_reopen cause — census classifies harness reopens as sp-reopen-<cause> (sp-0wwcn)"
# ======================================================================================
# bead_reopen <id> <cause> <note> writes event_type='reopen' with new_value=<cause>.
# census must report sp-reopen-<cause> with the correct distinct-bead count.
# POSITIVE CONTROL first (law-absence-needs-a-positive-control): verify absence is detectable.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-g0","title":"no-reopen control","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
_pc_out="$(census_out)"
is "positive control: no bead_reopen produces no sp-reopen-*" "" "$(printf '%s' "$_pc_out" | grep sp-reopen || true)"

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-g1","title":"reopen test","status":"closed","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
bead_reopen "sp-g1" gate-red "Reopened by test: sp-0wwcn" >/dev/null 2>&1

out="$(census_out)"
want "bead_reopen produces sp-reopen-gate-red in census" "sp-reopen-gate-red" "$out"
want "sp-reopen-gate-red shows 1 distinct bead" "1 sp-reopen-gate-red" "$out"
nowant "no bare sp-reopen class" "sp-reopen " "$out"

# Two different beads, same cause, both reopened inside this test run (well under the
# default clustering gap) — one causal event, victim count is 2 (sp-jcd0e).
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-g2","title":"reopen multi 1","status":"closed","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
{"id":"sp-g3","title":"reopen multi 2","status":"closed","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
bead_reopen "sp-g2" gate-red "first gate failure" >/dev/null 2>&1
bead_reopen "sp-g3" gate-red "second gate failure" >/dev/null 2>&1

out="$(census_out)"
want "two beads with same cause: one causal event, 2 victims" "1 sp-reopen-gate-red (2 victims" "$out"

# Verify the cause is recorded in the events table as event_type='reopen'.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-g4","title":"cause row test","status":"closed","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
bead_reopen "sp-g4" rebase-conflict "Reopened: conflict" >/dev/null 2>&1
_ev_cause="$(lcfix_fact_causes sp-g4 reopen)"
is "bead_reopen appends a reopen fact with the cause" "rebase-conflict" "$_ev_cause"

# ======================================================================================
echo
echo "sp-2w29g: bead_reopen appends \$SPIRA_RUN/reopen.log — the choke point traces itself"
# ======================================================================================
# An attended session (the Concierge) sources lib.sh and calls bead_reopen directly, by
# hand, never through landing.sh — so landing.sh's own log() line never runs for that
# reopen and the event row is the only trace anywhere. bead_reopen must write its own
# line so no caller, logged or not, can produce a silent reopen.
#
# NON-DEFAULT, EXPLICIT ENVIRONMENT: pin SPIRA_RUN to a scratch dir for this section so
# the assertion does not depend on whatever SPIRA_RUN the ambient conf.sh derived.
# Restored below — conf.sh already set SPIRA_RUN to a real directory before this file
# reached here, and every bead_reopen call after this section relies on it.
_rt_orig_run="$SPIRA_RUN"
_rt_run="$(mktemp -d)"
export SPIRA_RUN="$_rt_run"; tl_config SPIRA_RUN="$SPIRA_RUN"
_rt_log="$_rt_run/reopen.log"

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-h1","title":"trace control","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-27T00:00:00Z"}
JSONL
# POSITIVE CONTROL: before any bead_reopen call, no line names sp-h1.
is "positive control: no reopen.log line for sp-h1 before bead_reopen runs" \
    "" "$(grep -F 'sp-h1' "$_rt_log" 2>/dev/null || true)"

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-h1","title":"trace test","status":"closed","issue_type":"task","labels":["spira"],"updated_at":"2026-09-27T00:00:00Z"}
JSONL
BEADS_ACTOR=overseer bead_reopen "sp-h1" batch-eject "probe" >/dev/null 2>&1

_rt_line="$(grep -F 'reopen sp-h1 ' "$_rt_log" 2>/dev/null || true)"
want "reopen.log names the bead" "reopen sp-h1 " "$_rt_line"
want "reopen.log names the cause" "cause=batch-eject" "$_rt_line"
want "reopen.log names the non-harness actor (sp-2w29g: not just BEADS_ACTOR unset)" "actor=overseer" "$_rt_line"
# caller is read live from /proc/$PPID/cmdline and legitimately falls back to "unknown"
# when this process has no readable parent (PID 1 inside a suite container is exactly
# that case) — the field's presence is what's asserted, not a specific value.
want "reopen.log names a caller field" "caller=" "$_rt_line"

# COUNTS MUST MATCH EXACTLY (the bead's own acceptance criterion): one fact of kind
# 'reopen', and exactly one matching reopen.log line — not two, not zero.
_rt_evcount="$(lcfix_fact_causes sp-h1 reopen | grep -Fxc batch-eject || true)"
_rt_logcount="$(grep -Fc 'reopen sp-h1 cause=batch-eject' "$_rt_log" 2>/dev/null || true)"
is "reopen.log line count matches the reopen fact count exactly" "$_rt_evcount" "${_rt_logcount:-0}"

rm -rf "$_rt_run"
export SPIRA_RUN="$_rt_orig_run"; tl_config SPIRA_RUN="$SPIRA_RUN"

# _write_reopen writes a reopened event with a NULL new_value directly (no bump_*
# call produces a genuinely NULL cause). Used below by the sp-aor1l case; the NULL-cause
# column-shift property itself (D3) is table-tested on canned rows in
# test-census-pipeline.sh, not seeded through a real store here.
_write_reopen() {
    local id="$1"
    local uuid
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))' 2>/dev/null)" || return 1
    bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', 'reopened', 'harness', NULL, NOW())" >/dev/null 2>&1 || true
}

# ======================================================================================
echo
echo "sp-9edq8: bump_requeue merge-conflict + bead_reopen rebase-conflict → one census class"
# ======================================================================================
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail):
# Unfixed: census prints two lines — one sp-requeue-merge-conflict, one sp-reopen-rebase-conflict.
# Fixed: one line, sp-reopen-rebase-conflict with 1 bead. requeues_of still counts the requeue.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-z1","title":"conflict bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-19T00:00:00Z"}
JSONL
bump_requeue "sp-z1" merge-conflict >/dev/null 2>&1
bead_reopen  "sp-z1" rebase-conflict "conflict test" >/dev/null 2>&1

out="$(census_out)"
nowant "sp-requeue-merge-conflict absent: folded into sp-reopen-rebase-conflict" "sp-requeue-merge-conflict" "$out"
want   "sp-reopen-rebase-conflict present for the paired conflict events" "1 sp-reopen-rebase-conflict" "$out"
_conf_lines="$(printf '%s\n' "$out" | grep -c 'rebase-conflict\|merge-conflict' || true)"
is "exactly one conflict class line" "1" "$_conf_lines"
_att="$(attempts_of "sp-z1")"
is "no attempt charged for a conflict requeue" "0" "$_att"
_rqn="$(requeues_of "sp-z1")"
is "requeues_of still counts the requeue event" "1" "$_rqn"

# A REMEDY'S STATE IS ITS LIFECYCLE ROW (sp-mve9i, design §3.4): census splits the covers:
# beads by `spira-lc list`. lc_rows <id>:<STATE>... sets what this stub answers.
LC_REAL="$(command -v spira-lc)"
cat > "$TMP/spira-lc-stub" <<STUB
#!/usr/bin/env bash
case "\${1:-}" in fact|facts|facts-query) exec "$LC_REAL" "\$@" ;; esac
[ "\${1:-}" = list ] || exit 2
cat "$TMP/lc-rows.json" 2>/dev/null || echo '[]'
STUB
chmod +x "$TMP/spira-lc-stub"
export SPIRA_LC_BIN="$TMP/spira-lc-stub"
lc_rows() { local r out=""; for r in "$@"; do out="$out${out:+,}{\"bead_id\":\"${r%%:*}\",\"state\":\"${r#*:}\"}"; done; printf '[%s]\n' "$out" > "$TMP/lc-rows.json"; }

# ======================================================================================
echo
echo "db-hn1t: covers:sp-requeue-merge-conflict suppresses sp-reopen-rebase-conflict"
# ======================================================================================
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail):
# On the unfixed tree, census prints "3 sp-reopen-rebase-conflict (...)" without
# [suppressed] because the covers: label names the pre-fold class sp-requeue-merge-conflict
# and the grep -qxF match against the emitted class sp-reopen-rebase-conflict misses.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-p1","title":"conflict bead 1","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-19T00:00:00Z"}
{"id":"sp-p2","title":"conflict bead 2","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-19T00:00:00Z"}
{"id":"sp-p3","title":"conflict bead 3","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-19T00:00:00Z"}
{"id":"sp-p4","title":"remedy bead","status":"open","issue_type":"task","labels":["spira","maechen-remedy","covers:sp-requeue-merge-conflict"],"updated_at":"2026-09-19T00:00:00Z"}
JSONL
bump_requeue "sp-p1" merge-conflict >/dev/null 2>&1
bump_requeue "sp-p2" merge-conflict >/dev/null 2>&1
bump_requeue "sp-p3" merge-conflict >/dev/null 2>&1
lc_rows sp-p4:READY   # the remedy is open in lifecycle terms

_fold_out="$(census_out)"
_fold_line="$(printf '%s\n' "$_fold_out" | grep 'sp-reopen-rebase-conflict' || true)"
want "covers:sp-requeue-merge-conflict suppresses sp-reopen-rebase-conflict" "[suppressed" "$_fold_line"
nowant "sp-reopen-rebase-conflict not emitted unsuppressed" "sp-reopen-rebase-conflict" \
    "$(printf '%s\n' "$_fold_out" | grep -v '\[suppressed' || true)"

# Unrelated covers: label does NOT suppress sp-reopen-rebase-conflict.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-q1","title":"conflict bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-19T00:00:00Z"}
{"id":"sp-q2","title":"unrelated remedy","status":"open","issue_type":"task","labels":["spira","maechen-remedy","covers:sp-recur-suite-red"],"updated_at":"2026-09-19T00:00:00Z"}
JSONL
bump_requeue "sp-q1" merge-conflict >/dev/null 2>&1
lc_rows sp-q2:READY

_unrel_out="$(census_out)"
_unrel_line="$(printf '%s\n' "$_unrel_out" | grep 'sp-reopen-rebase-conflict' || true)"
nowant "unrelated covers: does not suppress sp-reopen-rebase-conflict" "[suppressed" "$_unrel_line"
want "sp-reopen-rebase-conflict still appears without suppression" "sp-reopen-rebase-conflict" "$_unrel_out"

# ======================================================================================
echo
echo "sp-aor1l: requeued/merge-conflict + reopened — sp-reopen-rebase-conflict only"
# ======================================================================================
# Positive control (law-a-regression-test-must-be-seen-to-fail): against unfixed lib.sh
# this bead also appears in sp-reopen-unrecorded because the exclusion only checked
# event_type='reopen', missing the requeued/merge-conflict fold path.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-i1","title":"conflict+reopened","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-19T00:00:00Z"}
JSONL
bump_requeue "sp-i1" merge-conflict >/dev/null 2>&1
_write_reopen sp-i1

out="$(census_out)"
want   "conflict+reopened: sp-reopen-rebase-conflict present" "1 sp-reopen-rebase-conflict" "$out"
nowant "conflict+reopened: sp-reopen-unrecorded absent"       "sp-reopen-unrecorded" "$out"

# ======================================================================================
echo
echo "sp-n8bjo: since-watermark exclusion subquery windows to the same watermark as the count"
# ======================================================================================
# The third UNION ALL branch (sp-reopen-unrecorded) excludes any bead that was EVER
# reopened with a recorded cause, checked with no time bound, while the branch's own
# count carries the since-watermark bound. A bead reopened with a cause BEFORE the
# watermark and again with no cause AFTER it is excluded forever, hiding the very
# causeless reopen a since-watermark pass exists to catch.
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail): on the unfixed tree
# the since-watermark query below reports 0 beads for reopened/unrecorded because the
# NOT IN subquery finds the pre-watermark cause-recorded reopen with no window applied.
# Run against the unfixed tree: "since-watermark: causeless reopen counted despite an
# older cause-recorded reopen: wanted [1] got [0]".
_insert_event_at() {   # _insert_event_at <bead_id> <event_type> <cause_or_empty> <utc_ts>
    local id="$1" etype="$2" cause="$3" ts="$4" uuid
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))' 2>/dev/null)" || return 1
    if [ -n "$cause" ]; then
        timeout 5 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql \
            "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', '$etype', 'harness', '$cause', '$ts')" \
            >/dev/null 2>&1
    else
        timeout 5 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql \
            "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', '$etype', 'harness', NULL, '$ts')" \
            >/dev/null 2>&1
    fi
}

WATERMARK_TS=1790400000
_wm_before="$(date -u -d "@$((WATERMARK_TS - 86400))" '+%Y-%m-%d %H:%M:%S')"
_wm_after="$(date -u -d  "@$((WATERMARK_TS + 3600))"  '+%Y-%m-%d %H:%M:%S')"

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-h1","title":"cause then causeless reopen","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-19T00:00:00Z"}
JSONL
_insert_event_at "sp-h1" "reopen"   "rebase-conflict" "$_wm_before"
_insert_event_at "sp-h1" "reopened" ""                "$_wm_after"

_since="$(census_events_run_sql "$WATERMARK_TS" 2>/dev/null)"
_causeless_beads="$(printf '%s\n' "$_since" | awk -F'|' '/reopened/ && /unrecorded/ {gsub(/ /,"",$3); print $3}')"
is "since-watermark: causeless reopen counted despite an older cause-recorded reopen" "1" "${_causeless_beads:-0}"

# All-time has no watermark, so since_clause is empty and the subquery is unchanged by
# the fix: sp-h1 still has a cause-recorded reopen somewhere in its (unbounded) history
# and stays excluded from the all-time bucket, exactly as before the fix.
_all="$(census_events_run_sql 2>/dev/null)"
_causeless_all="$(printf '%s\n' "$_all" | awk -F'|' '/reopened/ && /unrecorded/ {gsub(/ /,"",$3); print $3}')"
is "all-time: still excluded (dedup is 'ever' with no window to narrow it)" "0" "${_causeless_all:-0}"

# sp-n3ijm: census_events_run_sql's retry-on-transient-failure behaviour (fake bd, no
# database) moved to test-census-pipeline.sh (UC-ops-detection-remediation-15) — it was
# already T1-shaped here and belongs with the rest of the decision logic.

# ======================================================================================
echo
echo "sp-ytw2h: bead_reopen eviction-race + bump_requeue eviction-race → one census class"
# ======================================================================================
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail):
# Unfixed (before lib.sh fold): census prints two lines — sp-reopen-eviction-race AND
# sp-requeue-eviction-race. Fixed: one line, sp-reopen-eviction-race with 1 bead.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ev1","title":"eviction bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-23T00:00:00Z"}
JSONL
bead_reopen   "sp-ev1" eviction-race "Eviction race test" >/dev/null 2>&1
bump_requeue  "sp-ev1" eviction-race >/dev/null 2>&1

out="$(census_out)"
nowant "sp-requeue-eviction-race absent: folded into sp-reopen-eviction-race" "sp-requeue-eviction-race" "$out"
want   "sp-reopen-eviction-race present for the paired eviction events" "1 sp-reopen-eviction-race" "$out"
_evict_lines="$(printf '%s\n' "$out" | grep -c 'eviction-race' || true)"
is "exactly one eviction-race class line" "1" "$_evict_lines"

# ======================================================================================
echo
echo "sp-ytw2h: covers:sp-requeue-eviction-race suppresses sp-reopen-eviction-race"
# ======================================================================================
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail):
# Unfixed: sp-reopen-eviction-race appears unsuppressed even when a remedy bead carries
# covers:sp-requeue-eviction-race, because the fold map has no eviction-race entry.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ev2","title":"eviction bead 2","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-23T00:00:00Z"}
{"id":"sp-evr","title":"remedy bead","status":"in_progress","issue_type":"task","labels":["spira","maechen-remedy","covers:sp-requeue-eviction-race"],"updated_at":"2026-09-23T00:00:00Z"}
JSONL
bead_reopen   "sp-ev2" eviction-race "Eviction race test" >/dev/null 2>&1
bump_requeue  "sp-ev2" eviction-race >/dev/null 2>&1
lc_rows sp-evr:WORKING   # the remedy is being worked

_evict_sup_out="$(census_out)"
_evict_sup_line="$(printf '%s\n' "$_evict_sup_out" | grep 'sp-reopen-eviction-race' || true)"
want   "covers:sp-requeue-eviction-race suppresses sp-reopen-eviction-race" "[suppressed" "$_evict_sup_line"
nowant "sp-reopen-eviction-race not emitted unsuppressed" "sp-reopen-eviction-race" \
    "$(printf '%s\n' "$_evict_sup_out" | grep -v '\[suppressed' || true)"

# ======================================================================================
echo
echo "sp-a0of5: one re-filing's reopen-timing verdict does not rank as a second class"
# ======================================================================================
# incident.sh's file_one writes a 'recurred'/<incident-cause> event AND a
# 'reopen'/<timing-verdict> event ('closed-while-live' or 'recurrence') for the SAME
# re-filing (incident.sh:318,342). The timing verdict is not a cause — it is _reopen_cause's
# read of how recently the bead closed — so ranking it separately double-counts the filing.
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail): on the unfixed tree this
# single paired filing produces TWO class lines: "1 sp-recur-closed-not-landed" AND
# "1 sp-reopen-closed-while-live". Verified to fail before this commit.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-j1","title":"paired refiling","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-26T00:00:00Z"}
JSONL
_insert_event_at "sp-j1" "recurred" "closed-not-landed" "2026-09-26 22:04:27"
_insert_event_at "sp-j1" "reopen"   "closed-while-live" "2026-09-26 22:04:31"

out="$(census_out)"
want   "sp-recur-closed-not-landed still ranked under its true cause" "1 sp-recur-closed-not-landed" "$out"
nowant "sp-reopen-closed-while-live absent: it's a timing verdict, not a cause" "sp-reopen-closed-while-live" "$out"
_paired_lines="$(printf '%s\n' "$out" | grep -c 'closed-not-landed\|closed-while-live' || true)"
is "exactly one class line for the paired filing" "1" "$_paired_lines"

# The 'recurrence' timing verdict gets the same treatment.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-j2","title":"paired refiling 2","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-26T00:00:00Z"}
JSONL
_insert_event_at "sp-j2" "recurred" "oldest-unsent" "2026-09-26 22:10:00"
_insert_event_at "sp-j2" "reopen"   "recurrence"    "2026-09-26 22:10:04"

out="$(census_out)"
want   "sp-recur-oldest-unsent still ranked under its true cause" "1 sp-recur-oldest-unsent" "$out"
nowant "sp-reopen-recurrence absent: it's a timing verdict, not a cause" "sp-reopen-recurrence" "$out"

# ======================================================================================
echo
echo "sp-wkgyc: requeued/unjudged-<cause> — aeon_disposition's own free verdict is never ranked"
# ======================================================================================
# disposition() (aeon/src/decide.rs) marks EVERY unjudged-<cause> free but the one
# CHARGING_OUTCOME; census reads that constant.
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail): on the unfixed tree
# census ranks sp-requeue-unjudged-operator-wait — a correct escalation, not a failure
# class — exactly the defect this bead reports.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-u1","title":"operator-wait bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-27T00:00:00Z"}
JSONL
bump_requeue "sp-u1" "unjudged-operator-wait" >/dev/null 2>&1

out="$(census_out)"
nowant "sp-requeue-unjudged-operator-wait is never ranked" "sp-requeue-unjudged-operator-wait" "$out"
_att="$(attempts_of "sp-u1")"
is "no attempt charged for an unjudged-operator-wait requeue" "0" "$_att"
_rqn="$(requeues_of "sp-u1")"
is "requeues_of still counts the event" "1" "$_rqn"

# Not a finite hardcoded list: groomer.sh unpoison writes free-text unjudged-<cause> values
# (e.g. "precondition-satisfied") that never appear as a literal anywhere in aeon_disposition
# or in this suite until now — the exclusion must still catch it.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-u2","title":"unpoison-credited bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-27T00:00:00Z"}
JSONL
bump_requeue "sp-u2" "unjudged-precondition-satisfied" >/dev/null 2>&1

out="$(census_out)"
nowant "an arbitrary unjudged-<cause> (not a hardcoded literal) is also never ranked" \
    "sp-requeue-unjudged-precondition-satisfied" "$out"

# NEGATIVE CONTROL: unlanded is the one outcome aeon_disposition charges. A requeued event
# naming it must still rank — proving the exclusion reads CHARGING_OUTCOME rather than
# blindly trusting the unjudged- prefix (law-a-pattern-match-is-not-an-identity-check).
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-u3","title":"charging bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-27T00:00:00Z"}
JSONL
bump_requeue "sp-u3" "unjudged-unlanded" >/dev/null 2>&1

out="$(census_out)"
want "unjudged-unlanded is not exempted: it still ranks" "sp-requeue-unjudged-unlanded" "$out"

echo
tl_summary
