#!/usr/bin/env bash
#
# test-census.sh — census.sh against a real bd fixture: the one server-mode suite for
#   UC-ops-detection-remediation-07, -12 and -14 (law-prefer-the-real-dependency).
#
#   ./test-census.sh
#
# WHAT THIS TESTS
# ---------------
# Everything census.sh can DECIDE (class mapping, ranking, suppression annotation) is
# table-tested on canned rows in test-census-pipeline.sh, with no database. What is left
# here is only the wiring that a canned-row test cannot reach: that bump_*/bead_reopen
# really write rows the real SQL reads, that a watermark file really narrows the real
# query, and that the open/closed-unlanded/landed remedy states really come from a real
# bd list against a real store.
#
# tier: T3
# covers: census/src/* spira/census/deliberate.py spira/lib.sh UC-ops-detection-remediation-07 UC-ops-detection-remediation-12 UC-ops-detection-remediation-14
# hermetic-ok: uses a fixture database; no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
lack() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-census
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — census.sh reads counts through census_events_run_sql (bd sql), which embedded mode refuses
export SPIRA_TESTDB_MODE=server
testdb_up census || {
    printf 'SKIP test-census: server testdb not available\n' >&2
    exit 77
}

# Source lib.sh for bump_requeue/bead_reopen and the shared _bump_write_event helper.
# These write the event rows census.sh reads; adding labels via 'bd label add' only
# creates 'label_added' events, which census never queries. Protect SPIRA_DB since
# lib.sh re-sources conf.sh.
_PRE_LIB_SPIRA_DB="$SPIRA_DB"
# shellcheck disable=SC1090
. "$HERE/lib.sh"
SPIRA_DB="$_PRE_LIB_SPIRA_DB"; unset _PRE_LIB_SPIRA_DB

# bump_recur/bump_reclaim (lib.sh) were retired at sp-8itaf — zero live callers; the
# production writers are now incident::ports::bump_recur and strand::check::bump_reclaim
# (Rust). This suite's own subject is census reading the events table, not who writes
# it, so these two local wrappers reach the same shared writer lib.sh's versions did.
recur_event()   { _bump_write_event "${1:-}" recurred  "${2:-unrecorded}"; }
reclaim_event() { _bump_write_event "${1:-}" reclaimed "${2:-unrecorded}"; }

CENSUS="$(command -v census)"
B() { bd -C "$SPIRA_DB" "$@"; }
REMEDY_LABEL=maechen-remedy

run_census() {
    env SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL="$REMEDY_LABEL" \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="${_CENSUS_RUN:-$TMP/no-run}" \
        "$CENSUS" "$@" 2>/dev/null
}

run_census_repo() {  # run_census_repo <repo-path> [census-args...]
    local _rp="$1"; shift
    env SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL="$REMEDY_LABEL" \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$_rp" \
        "$CENSUS" "$@" 2>/dev/null
}

plant_bead() {  # plant_bead <title> → bead id on stdout
    B create "$1" --type bug --priority 2 --labels spira,incident --silent 2>/dev/null \
        | tr -d '[:space:]'
}

echo "test-census.sh"

# ==============================================================================
echo
echo "1. bump_* writes -> census reads (real events table, distinct beads not events)"
# ==============================================================================
# POSITIVE CONTROL (law-absence-needs-a-positive-control): an empty store first, to
# show absence is detectable before relying on it.
testdb_reset
empty_out="$(run_census)"
is "positive control: empty store reports nothing" "" "$empty_out"

bid_a="$(plant_bead "bead-a")"
[ -n "$bid_a" ] || { bad "bead-a created" "create failed"; tl_summary; exit; }
recur_event "$bid_a" suite-red; recur_event "$bid_a" suite-red; recur_event "$bid_a" suite-red

bid_b="$(plant_bead "bead-b")"
recur_event "$bid_b" suite-red

bid_c="$(plant_bead "bead-c")"
bump_requeue "$bid_c" quota-exceeded
reclaim_event "$bid_c"

out1="$(run_census)"
want "bump_recur: 2 distinct beads, 4 detections" "2 sp-recur-suite-red (4" "$out1"
want "bump_requeue: 1 distinct bead, 1 detection"  "1 sp-requeue-quota-exceeded (1" "$out1"
want "bump_reclaim: 1 distinct bead, 1 detection"  "1 sp-reclaim (1"          "$out1"

# UC-ops-detection-remediation-07's Sin-escalation recurrence counter is now
# incident::ports::recurs_of (Rust, cargo test -p incident) rather than a lib.sh
# accessor — the decision itself (threshold, exempt refs, DRAINING) is table-tested
# against a stub bd in test-sin-exempt.sh. What stays here is proof that census.sh
# itself counts the real 'recurred' events bid_a's three calls above just wrote,
# which the assertions above already establish.

# ==============================================================================
echo
echo "2. bead_reopen cause column (sp-0wwcn)"
# ==============================================================================
testdb_reset
bid_g="$(plant_bead "reopen-cause-bead")"
bead_reopen "$bid_g" gate-red "Reopened by test: sp-0wwcn" >/dev/null 2>&1

_ev_cause="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql \
    "SELECT COALESCE(new_value,'') FROM events WHERE issue_id='$bid_g' AND event_type='reopen'" \
    2>/dev/null | sed -n '3p' | tr -d ' ')"
is "bead_reopen writes event_type=reopen with cause in new_value" "gate-red" "$_ev_cause"

out2="$(run_census)"
want "census reports sp-reopen-gate-red from the real reopen row" "1 sp-reopen-gate-red" "$out2"

# ==============================================================================
echo
echo "2b. sp-eiatd: queue.sh eject is a deliberate reopen cause — counted, never ranked"
# ==============================================================================
# The census aeon's own evidence (sp-eiatd): a correctly-attributed queue.sh eject is the
# system working (law-a-deliberate-state-is-not-a-fault), not a failure class. Before the
# fix, sp-reopen-eject competed for the top unsuppressed Maechen slot alongside genuine
# defects. PAIRED WITH A NON-DELIBERATE CAUSE (law-absence-needs-a-positive-control): if
# this just emptied the ranked list rather than discriminating on cause, gate-red would
# vanish too — it must not.
testdb_reset
bid_ej="$(plant_bead "eject-deliberate-bead")"
bead_reopen "$bid_ej" eject "" >/dev/null 2>&1

bid_gr="$(plant_bead "gate-red-still-ranks-bead")"
bead_reopen "$bid_gr" gate-red "" >/dev/null 2>&1

out2b="$(run_census)"
lack "eject: excluded from the default ranked census" "sp-reopen-eject" "$out2b"
want "gate-red: still ranks (discriminating positive control)" "1 sp-reopen-gate-red" "$out2b"

out2b_sup="$(run_census --with-suppressed)"
want "eject: still counted under --with-suppressed" "deliberate, not ranked: sp-reopen-eject 1 beads" "$out2b_sup"
lack "eject: never appears in the ranked block itself, even with --with-suppressed" \
    "1 sp-reopen-eject" "$(printf '%s\n' "$out2b_sup" | grep -v 'deliberate, not ranked' || true)"

# ==============================================================================
echo
echo "3. since-filter reaches the real SQL (watermark narrows the query)"
# ==============================================================================
# A stale event (before the watermark) and a live one (after) — the since-watermark
# query must see only the live one, proving SPIRA_RUN/maechen.watermark really reaches
# census_events_run_sql's since-clause and not just merge.py's ranking (table-tested
# separately in test-census-pipeline.sh).
testdb_reset
bid_stale="$(plant_bead "stale-watermark-bead")"
_uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))')"
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql \
    "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$_uuid', '$bid_stale', 'recurred', 'harness', 'stale-class', FROM_UNIXTIME(1000000000))" \
    >/dev/null 2>&1

bid_live="$(plant_bead "live-watermark-bead")"
recur_event "$bid_live" live-class

_CENSUS_RUN="$TMP/wm-run"; mkdir -p "$_CENSUS_RUN"
printf '1500000000\n' > "$_CENSUS_RUN/maechen.watermark"
out3="$(run_census)"
unset _CENSUS_RUN
want "since-filter: live class counted since the watermark" "1 sp-recur-live-class (1 detections, 1 all-time)" "$out3"
want "since-filter: stale class shows 0 since watermark, 1 all-time" \
    "0 sp-recur-stale-class (0 detections, 1 all-time)" "$out3"

# ==============================================================================
echo
echo "4. open remedy bead -> class suppressed, --with-suppressed annotates it"
# ==============================================================================
testdb_reset
bid_r="$(plant_bead "remedy-target-bead")"
recur_event "$bid_r" remedy-class

remedy_id="$(B create "Fix sp-recur-remedy-class" --type task --priority 2 \
    --labels "spira,plan,${REMEDY_LABEL},covers:sp-recur-remedy-class" \
    --silent 2>/dev/null | tr -d '[:space:]')"
[ -n "$remedy_id" ] || { bad "remedy bead created" "create failed"; tl_summary; exit; }

out4="$(run_census)"
lack "open remedy: class excluded by default" "sp-recur-remedy-class" "$out4"

out4s="$(run_census --with-suppressed)"
want "open remedy: --with-suppressed shows the class" "sp-recur-remedy-class" "$out4s"
want "open remedy: annotated [suppressed]" "[suppressed]" "$out4s"

# ==============================================================================
echo
echo "5. closed-unlanded remedy keeps suppressing; landing reinstates the class"
# ==============================================================================
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail): closing a remedy bead
# must not lift suppression until its commit is actually on the base.
FIXTURE_REPO="$TMP/fixture-repo"
git init -q "$FIXTURE_REPO" \
    && GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=test \
       GIT_COMMITTER_EMAIL=t@t \
       git -C "$FIXTURE_REPO" commit --allow-empty -q -m "initial" 2>/dev/null

run_census_fixture() {
    env SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL="$REMEDY_LABEL" \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$FIXTURE_REPO" \
        "$CENSUS" "$@" 2>/dev/null
}

B close "$remedy_id" --reason "test: verify closed remedy still suppresses" --force >/dev/null 2>&1
# A closed remedy with NO branch is orphaned (sp-c3q60) — that decision path is
# table-tested (fake bd, real throwaway git) in test-census-pipeline.sh. This fixture
# gives it a branch to represent the in-flight case.
git -C "$FIXTURE_REPO" branch "spira/${remedy_id}" >/dev/null 2>&1

out5_pre="$(run_census_fixture)"
lack "closed-unlanded: still suppressed" "sp-recur-remedy-class" "$out5_pre"
want "closed-unlanded: annotated [suppressed: remedy closed, not landed]" \
    "[suppressed: remedy closed, not landed]" "$(run_census_fixture --with-suppressed)"

# Land the remedy: a landing-record commit naming the bead on the base. landed() trusts
# only two subject shapes (law-a-matcher-reads-code-not-prose / sp-dgaig); a
# cross-reference like "fix: <id> closes <class>" is a mention, not a landing record.
GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=t@t \
    git -C "$FIXTURE_REPO" commit --allow-empty -q \
    -m "spira: land $remedy_id" 2>/dev/null

out5="$(run_census_fixture)"
want "landed: class reappears" "sp-recur-remedy-class" "$out5"
lack "landed: no suppression annotation" "[suppressed]" "$out5"

echo
tl_summary
