#!/usr/bin/env bash
# test-release-acceptance.sh — structural checks on acceptance-run.sh:
# the script exists, accepts required arguments, and has the structural
# properties a PASS/FAIL acceptance runner must have.
#
# This suite verifies the MECHANISM, not the runtime result — the runtime result
# requires a clean machine with real bd/dolt/gh/claude and is what the operator
# confirms. A structural test that cannot find the script, or finds one that
# lacks a verdict line, is catching the gap before the operator discovers it.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# --------------------------------------------------------
# Case 1 proves the script is present before any "not found" result counts as
# evidence. A missing script and a check that never matches look identical from
# outside the test.
#
# CASES
#   1. POSITIVE CONTROL: acceptance-run.sh exists and is executable.
#   2. Usage error: no arguments exits non-zero without running.
#   3. Missing --scratch-repo: exits non-zero with a usage message.
#   4. PASS/FAIL verdict: script emits "verdict: PASS" or "verdict: FAIL".
#   5. Positive-control section: script has a positive-control self-check.
#   6. Ancestry verification: script uses merge-base or ancestry, not bead status.
#   7. --record writes a git note (structure: git notes --ref=acceptance).
#   8. Phase A, B, C, D: script has all four phase labels.
#   9. Upgrade assertions: .tag sidecar and SPIRA_PROD checks present.
#  10. Rollback unit-set diff: diff of unit sets is present.
#  11. Phase D: aged-install upgrade — bead/memory count, doctor, operator override,
#      crash-loop check, world-running check, post-upgrade ancestry landing.
#  12. Phase D: rollback either refused (names migration) or succeeds with healthy world.
#  13. Phase D: (from, to) pair recorded in git note when --prev-tag is given.
#  14. acceptance-agent.sh exists and is executable.
#  15. ready.sh capture: correct exit-capture pattern, not || true.
#  16. Phase A and D: CONFIGURE_PROD set to harness subdir (not clone root).
#  17. Phase A: SPIRA_AGENT written before install.sh (not gated on _install_rc).
#  18. Phase A and D: ready.sh call uses clone path, not workspace path.
#  19. Fixture: clone's ready.sh governs check, not workspace's.
#  20. Phase A: bead labels carry plan+scope so builder predicate matches.
#  21. Phase A: claimability check uses bd ready, not sentinel --report polling.
#
# covers: spira/acceptance-run.sh spira/acceptance-agent.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SCRIPT="$HERE/acceptance-run.sh"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { grep -qF "$2" "$SCRIPT" 2>/dev/null && ok "$1" || bad "$1" "not found: $2"; }
wantre() { grep -qE "$2" "$SCRIPT" 2>/dev/null && ok "$1" || bad "$1" "pattern not found: $2"; }

echo "test-release-acceptance.sh"

# ============================================================================
echo
echo "1. POSITIVE CONTROL — acceptance-run.sh exists and is executable"
# ============================================================================

if [ -f "$SCRIPT" ] && [ -x "$SCRIPT" ]; then
    ok "acceptance-run.sh exists and is executable"
else
    bad "acceptance-run.sh exists and is executable" "missing or not executable"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

# ============================================================================
echo
echo "2. Usage error exits non-zero with no arguments"
# ============================================================================

bash "$SCRIPT" >/dev/null 2>&1 && \
    bad "no-arg invocation exits non-zero" "exit 0 (expected non-zero)" \
    || ok "no-arg invocation exits non-zero"

# ============================================================================
echo
echo "3. Missing --scratch-repo exits non-zero"
# ============================================================================

bash "$SCRIPT" "some-tag" >/dev/null 2>&1 && \
    bad "missing --scratch-repo exits non-zero" "exit 0 (expected non-zero)" \
    || ok "missing --scratch-repo exits non-zero"

# ============================================================================
echo
echo "4. PASS/FAIL verdict line"
# ============================================================================

want "script emits a verdict line" "verdict:"
want "verdict includes PASS or FAIL"  'verdict="FAIL"'
want "verdict reports PASS on zero failures" 'verdict="PASS"'

# ============================================================================
echo
echo "5. Positive-control self-check"
# ============================================================================

want "positive-control self-check present" "positive-control: command -v catches missing tool"

# ============================================================================
echo
echo "6. Ancestry verification (not status)"
# ============================================================================

# The bead says to verify landing by ancestry, not by the bead's status.
# git log with a SHA range, or merge-base --is-ancestor, are both valid ancestry checks.
wantre "ancestry check uses git log range or merge-base" "merge-base.*is-ancestor|git.*log.*\\.\\.|_base_sha_before\\.\\."
# Must NOT check landing only via bd show / bd close status.
if grep -qE "bd.*show.*\$_bead_id|bead_status" "$SCRIPT" 2>/dev/null; then
    bad "landing check does not rely on bead status" \
        "script uses bd show / status to check landing (ancestry check required)"
else
    ok "landing check does not rely solely on bead status"
fi

# ============================================================================
echo
echo "7. --record writes a git note under refs/notes/acceptance"
# ============================================================================

want "--record flag writes a git note"         "git notes --ref=acceptance"
want "--record flag uses refs/tags/<tag>"      "refs/tags/"

# ============================================================================
echo
echo "8. Phase A, B, C, D labels present"
# ============================================================================

want "phase A label present" "phase A"
want "phase B label present" "phase B"
want "phase C label present" "phase C"
want "phase D label present" "phase D"

# ============================================================================
echo
echo "9. Upgrade assertions: .tag sidecar and SPIRA_PROD"
# ============================================================================

# sp-cb0q1: deploy.sh writes .tag sidecar; acceptance-run checks it.
want "phase B checks .tag sidecar" ".tag"
want "phase B checks SPIRA_PROD updated" "SPIRA_PROD"

# ============================================================================
echo
echo "10. Rollback unit-set diff"
# ============================================================================

# sp-x6ygl: rollback must restore the prior release's unit set exactly.
want "phase C captures pre-upgrade unit set"  "_units_pre_upgrade"
want "phase C captures post-rollback unit set" "_units_post_rollback"
want "phase C diffs pre vs post"               "_unit_diff"

# ============================================================================
echo
echo "11. Phase D: aged-install upgrade checks present"
# ============================================================================

# Bead and memory counts preserved through migration.
want "phase D checks bead count preserved"     "_aged_pre_beads"
want "phase D checks memory count preserved"   "_aged_pre_mems"

# Doctor check.
want "phase D runs doctor.sh"                  "_aged_doctor_rc"

# Operator override survives.
want "phase D checks operator override"        "ACCEPTANCE_AGED_OVERRIDE"

# No crash-loop: failed units check.
want "phase D checks failed units"             "_aged_failed"

# World running after upgrade.
want "phase D checks world not halted"         "_aged_world_out"

# Post-upgrade bead landing by ancestry.
want "phase D files post-upgrade bead"         "_aged_probe_id"
want "phase D waits for post-upgrade landing"  "_aged_landed"
wantre "phase D ancestry check on post-upgrade bead" '_aged_land_base.*\.\.'

# ============================================================================
echo
echo "12. Phase D: rollback handled — refused names migration, or succeeds with healthy world"
# ============================================================================

want "phase D rollback refused must name migration" "migrat"
want "phase D rollback succeeded: world check"      "_aged_rollback_world"

# ============================================================================
echo
echo "13. Phase D: (from, to) pair recorded in git note"
# ============================================================================

want "aged-install result in git note" "aged-install from="

# ============================================================================
echo
echo "14. acceptance-agent.sh exists and is executable"
# ============================================================================

AGENT="$HERE/acceptance-agent.sh"
wanta() { grep -qF "$2" "$AGENT" 2>/dev/null && ok "$1" || bad "$1" "not found: $2"; }

if [ -f "$AGENT" ] && [ -x "$AGENT" ]; then
    ok "acceptance-agent.sh exists and is executable"
else
    bad "acceptance-agent.sh exists and is executable" \
        "missing or not executable at $AGENT"
fi
wanta "acceptance-agent.sh drains stdin"  "cat >/dev/null"
wanta "acceptance-agent.sh commits probe" "acceptance-probe.txt"
wanta "acceptance-agent.sh closes bead"   "close"

# ============================================================================
echo
echo "15. ready.sh capture: correct pattern, not || true"
# ============================================================================
# The old code used _ready_out=$(bash ready.sh) || true; _ready_rc=$? which
# always gave _ready_rc=0 because || true runs last (law-status-after-a-pipe-is-the-last-command).
wantre "ready.sh: _ready_rc initialized before capture" \
    '_ready_rc=0'
wantre "ready.sh: exit captured with || _ready_rc=\\\$\\?" \
    'bash.*ready\.sh.*\|\| _ready_rc=\$\?'
wantre "ready.sh: _ready_out printed on failure" \
    '_ready_rc.*-eq 0.*\|\|.*printf.*_ready_out'
# The buggy || true must not appear on the same line as ready.sh.
if grep -E 'ready\.sh.*\|\| true' "$SCRIPT" 2>/dev/null; then
    bad "ready.sh capture does not use || true" \
        "found ready.sh line with || true — exit code always 0"
else
    ok "ready.sh capture does not use || true"
fi

# ============================================================================
echo
echo "16. Phase A and D: CONFIGURE_PROD set to harness subdir (not clone root)"
# ============================================================================
want "phase A CONFIGURE_PROD includes /spira suffix" 'CONFIGURE_PROD=$_clone/spira'
want "phase D CONFIGURE_PROD includes /spira suffix" 'CONFIGURE_PROD=$_aged_clone/spira'

# ============================================================================
echo
echo "17. Phase A: SPIRA_AGENT written before install.sh (not gated on _install_rc)"
# ============================================================================
# Pre-seeding the conf before install.sh means SPIRA_AGENT survives even when
# install exits non-zero (e.g. exit 3: installed but not ready).
# install.sh phase 1 sees the file and skips configure.sh; the values persist.
wantre "phase A pre-seeds conf before install.sh call" \
    'printf.*SPIRA_AGENT|SPIRA_OPERATED.*>.*_conf'
# The conf write must NOT be inside an 'if _install_rc' guard.
if grep -A5 '_install_rc.*-eq 0' "$SCRIPT" 2>/dev/null \
        | grep -q 'SPIRA_AGENT'; then
    bad "SPIRA_AGENT write not gated on _install_rc" \
        "SPIRA_AGENT write appears inside an _install_rc check — must be pre-seeded"
else
    ok "SPIRA_AGENT write not gated on _install_rc"
fi

# ============================================================================
echo
echo "18. Phase A and D: ready.sh call uses clone path, not workspace path"
# ============================================================================
# The old code called bash "\$HERE/ready.sh" which read workspace paths; the
# clone's own ready.sh (with clone paths) was never consulted, so install-side
# fixes to ready.sh never changed the acceptance verdict.
wantre "phase A calls clone's ready.sh" \
    'bash.*\$_clone/spira/ready\.sh'
wantre "phase A failure names clone's ready.sh path" \
    'bad.*\$_clone.*ready\.sh'
wantre "phase D calls aged clone's ready.sh" \
    'bash.*\$_aged_clone/spira/ready\.sh'
wantre "phase D failure names aged clone's ready.sh path" \
    'bad.*\$_aged_clone.*ready\.sh'
# Workspace's ready.sh must not appear on the capture line.
if grep -E 'bash.*\$HERE/ready\.sh' "$SCRIPT" 2>/dev/null; then
    bad "ready.sh call does not use workspace path" \
        "found bash \$HERE/ready.sh — must use clone path"
else
    ok "ready.sh call does not use workspace path"
fi

# ============================================================================
echo
echo "19. Fixture: clone's ready.sh governs check, not workspace's"
# ============================================================================
# Positive: stub clone ready.sh exits 0, workspace exits 1 → check passes.
# Negative: stub clone ready.sh exits 1, workspace exits 0 → check fails naming clone's path.

_fix_tmp="$(mktemp -d)"

mkdir -p "$_fix_tmp/clone/spira"
printf '#!/bin/sh\nexit 0\n' > "$_fix_tmp/clone/spira/ready.sh"
chmod +x "$_fix_tmp/clone/spira/ready.sh"

_fix_out="$(
    _clone="$_fix_tmp/clone"
    _ready_rc=0
    _ready_out="$(bash "$_clone/spira/ready.sh" 2>&1)" || _ready_rc=$?
    if [ "$_ready_rc" -eq 0 ]; then
        printf 'ok phase A: ready.sh exits 0 after install\n'
    else
        printf 'FAIL phase A: ready.sh exits 0 after install: %s exit %d\n' \
            "$_clone/spira/ready.sh" "$_ready_rc"
    fi
    [ "$_ready_rc" -eq 0 ] || printf '%s\n' "$_ready_out"
)"

echo "$_fix_out" | grep -q '^ok ' \
    && ok "fixture: clone ready.sh=0 → check passes" \
    || bad "fixture: clone ready.sh=0 → check passes" "$(echo "$_fix_out" | head -1)"

# Negative: clone exits 1 → fail line must name clone's path.
printf '#!/bin/sh\nprintf "not ready\n"\nexit 1\n' > "$_fix_tmp/clone/spira/ready.sh"

_fix_out2="$(
    _clone="$_fix_tmp/clone"
    _ready_rc=0
    _ready_out="$(bash "$_clone/spira/ready.sh" 2>&1)" || _ready_rc=$?
    if [ "$_ready_rc" -eq 0 ]; then
        printf 'ok phase A: ready.sh exits 0 after install\n'
    else
        printf 'FAIL phase A: ready.sh exits 0 after install: %s exit %d\n' \
            "$_clone/spira/ready.sh" "$_ready_rc"
    fi
    [ "$_ready_rc" -eq 0 ] || printf '%s\n' "$_ready_out"
)"

echo "$_fix_out2" | grep -qF "FAIL phase A: ready.sh exits 0 after install: $_fix_tmp/clone" \
    && ok "fixture: clone ready.sh=1 → check fails naming clone's path" \
    || bad "fixture: clone ready.sh=1 → check fails naming clone's path" \
        "$(echo "$_fix_out2" | head -1)"

rm -rf "$_fix_tmp"

# ============================================================================
echo
echo "20. Phase A: bead labels carry plan+scope so builder predicate matches"
# ============================================================================
# Root cause of sp-utdzv: bead was filed with 'acceptance,repo:scratch-repo'
# but the builder reads FAYTH_LABELS="${scope},${plan}". A bead missing those
# labels is visible to the sentinel but claimable by no persona — indistinguishable
# from a sentinel not running (law-absence-needs-a-positive-control).
#
# The fix: read SPIRA_PLAN_LABEL and SPIRA_SCOPE_LABEL from the installed conf
# and add them to the bead creation call.
want "phase A reads plan label from installed conf" '_a_plan_label'
want "phase A reads scope label from installed conf" '_a_scope_label'
want "phase A bead creation uses plan-label variable" '"acceptance,${_a_plan_label}'
want "phase A bead creation uses scope-label variable" '"acceptance,${_a_plan_label},${_a_scope_label}'

# ============================================================================
echo
echo "21. Phase A: claimability check uses bd ready, not sentinel --report polling"
# ============================================================================
# sentinel --report only lists SPIRA_GOAL children; a bead not under the goal epic
# never appears there. The old 6-minute polling loop timed out on every run because
# the acceptance bead was never filed as a goal child. A bd ready check is
# immediate and directly tests whether the builder predicate matches the bead.
wantre "phase A claimability check uses bd ready" 'bd.*ready.*_a_scope_label.*_a_plan_label|bd.*ready.*_a_plan_label.*_a_scope_label|bd.*ready.*label.*_a'
want "phase A bead-not-claimable error names predicate" 'builder predicate does not match bead labels'

# ============================================================================
echo
echo "22. Phase B: install.sh output not discarded on failure"
# ============================================================================
# Phase B previously sent all install.sh output to /dev/null, so a failing
# install left only an exit code and no cause. The fix streams via tee, as
# phase A and D do.

if grep -E 'bash.*_prev_clone.*install\.sh.*>/dev/null' "$SCRIPT" 2>/dev/null; then
    bad "phase B install.sh output not discarded" \
        "found install.sh with >/dev/null — output invisible on failure"
else
    ok "phase B install.sh output not discarded"
fi
wantre "phase B install.sh streams output (tee pattern)" \
    '_prev_clone.*install\.sh.*tee|tee.*prev-install'

# Fixture: a failing phase B install.sh must emit its output before the FAIL line.
_pb_tmp="$(mktemp -d)"
mkdir -p "$_pb_tmp/clone"
printf '#!/bin/sh\nprintf "prev-install-failure-reason\n"\nexit 1\n' \
    > "$_pb_tmp/clone/install.sh"
chmod +x "$_pb_tmp/clone/install.sh"

_pb_out="$(
    _prev_clone="$_pb_tmp/clone"
    _prev_install_rc=0
    SPIRA_OPERATED=0 bash "$_prev_clone/install.sh" 2>&1 | tee "$_pb_tmp/prev-install.log" \
        || _prev_install_rc=$?
    [ "$_prev_install_rc" -ne 0 ] && \
        printf '  FAIL  phase B: install.sh exits 0: exit %d\n' "$_prev_install_rc" \
        || true
)"

printf '%s\n' "$_pb_out" | grep -q 'prev-install-failure-reason' \
    && ok "fixture: phase B failing install.sh output reaches report" \
    || bad "fixture: phase B failing install.sh output reaches report" \
        "marker not found; got: $(printf '%s\n' "$_pb_out" | head -3)"

rm -rf "$_pb_tmp"

# ============================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
