#!/usr/bin/env bash
#
# test-verdict-action.sh — the pure classifier behind verdict.sh's pending and
# harness_fault branches: verdict_action, verdict_parse_run_metadata,
# verdict_repo_threshold and verdict_normalize_status. No forge, no git, no bd —
# every input is a plain string or a pre-computed age in seconds.
#
# Demoted from test-verdict.sh cases 3, 12, 14, 17, 18, 27 (docs/test-plan/
# landing-merge-queue.md UC-43, decision half of UC-44): those cases each paid
# for a full verdict.sh process over a live git repo and forge fixture to
# observe one classification decision. Cases 1 and 3 stay in test-verdict.sh as
# assembly controls that the real pipeline still wires to this classifier.
#
# tier: T1
# covers: spira/verdict.sh UC-landing-merge-queue-43 UC-landing-merge-queue-44
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-verdict-action.sh"

# shellcheck disable=SC1090
. "$HERE/verdict.sh"

# =============================================================================
# 1. UNKNOWN AGE (positive control): a run exists but its start time could not
#    be determined — never cancel on a guess.
# =============================================================================
is "1. unknown age: wait" "wait-unknown" "$(verdict_action pending "" "" 3600 0 600)"

# =============================================================================
# 2. Demoted case 3 — PENDING, RUN OLD AND STUCK: age past maxsec, no activity
#    at all → cancel.
# =============================================================================
is "2. past-maxsec, no activity: cancel" "cancel" \
    "$(verdict_action pending 3601 "" 3600 0 600)"

# =============================================================================
# 3. Demoted case 12 — PR OLD, RUN FRESHLY STARTED: age well under maxsec, even
#    though the batch itself is old → wait-running, not cancel.
#    Regression this guards: verdict.sh used to age the PR instead of the run.
# =============================================================================
is "3. fresh run under maxsec: wait-running" "wait-running" \
    "$(verdict_action pending 60 "" 3600 0 600)"

# =============================================================================
# 4. Demoted case 14 — PR OLD, RUN OLD, LAST-ACTIVITY RECENT: age past maxsec,
#    but a step completed inside idle_max → wait-progressing, not cancel.
# =============================================================================
is "4. past-maxsec but idle recent: wait-progressing" "wait-progressing" \
    "$(verdict_action pending 3700 30 3600 0 600)"

# POSITIVE CONTROL for case 4: the same run age with idle past idle_max cancels.
is "4b. past-maxsec and idle stale: cancel" "cancel" \
    "$(verdict_action pending 3700 700 3600 0 600)"

# =============================================================================
# 5. Demoted case 17 — SINGLE-STEP JOB: forge emits two last-activity lines (one
#    from run.updated_at, recent; one from a stale single step). The max of the
#    two must be taken, or the stale one alone would read as stuck.
# =============================================================================
now="$(date +%s)"
meta="started-at: $(( now - 3700 ))
last-activity: $(( now - 30 ))
last-activity: $(( now - 3700 ))"
parsed="$(verdict_parse_run_metadata "$meta")"
want "5. parse: started line present" "started=$(( now - 3700 ))" "$parsed"
want "5. parse: max last-activity taken" "last_activity=$(( now - 30 ))" "$parsed"

# =============================================================================
# 6. Demoted case 18 — PER-REPO CI_MAXSEC OVERRIDE: SPIRA_QUEUE_CI_MAXSEC_<NAME>
#    is used when set, else the global, else the default.
# =============================================================================
unset SPIRA_QUEUE_CI_MAXSEC SPIRA_QUEUE_CI_MAXSEC_FIXTURE_REPO
is "6. no override: default 3600" "3600" \
    "$(verdict_repo_threshold fixture-repo CI_MAXSEC 3600)"
SPIRA_QUEUE_CI_MAXSEC=1800
is "6b. global override" "1800" \
    "$(verdict_repo_threshold fixture-repo CI_MAXSEC 3600)"
SPIRA_QUEUE_CI_MAXSEC_FIXTURE_REPO=60
is "6c. per-repo override beats global" "60" \
    "$(verdict_repo_threshold fixture-repo CI_MAXSEC 3600)"
unset SPIRA_QUEUE_CI_MAXSEC SPIRA_QUEUE_CI_MAXSEC_FIXTURE_REPO
# Same age (120s) decides differently depending on which threshold won above.
is "6d. age 120s under global-1800: wait-running" "wait-running" \
    "$(verdict_action pending 120 "" 1800 0 600)"
is "6e. age 120s over per-repo-60: cancel" "cancel" \
    "$(verdict_action pending 120 "" 60 0 600)"

# =============================================================================
# 7. Demoted case 27 — PROVISION FAULT: normalized to harness_fault before the
#    retry decision, and treated identically to a real harness_fault.
# =============================================================================
is "7. provision_fault normalizes" "harness_fault" \
    "$(verdict_normalize_status provision_fault)"
is "7b. harness_fault unaffected" "harness_fault" \
    "$(verdict_normalize_status harness_fault)"
is "7c. other statuses unaffected" "green" \
    "$(verdict_normalize_status green)"
is "7d. provision_fault, retries remaining: rerun" "rerun" \
    "$(verdict_action "$(verdict_normalize_status provision_fault)" "" "" 0 0 0 2)"

# =============================================================================
# 8. Demoted case 4/5 decision — harness_fault retry budget: rerun while under
#    max_retries, close once exhausted.
# =============================================================================
is "8. retries under budget: rerun" "rerun" \
    "$(verdict_action harness_fault "" "" 0 1 0 2)"
is "8b. retries exhausted: close" "close" \
    "$(verdict_action harness_fault "" "" 0 2 0 2)"

tl_summary
