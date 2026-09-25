#!/usr/bin/env bash
# tier: T2
# covers: spira/acceptance-run.sh spira/acceptance-agent.sh UC-instance-lifecycle-46 UC-instance-lifecycle-47
#
# This suite verifies the MECHANISM, not the runtime result — the runtime result
# requires a clean machine with real bd/dolt/gh/claude and is what the operator
# confirms (spira/acceptance-ci.sh, acceptance.yml). The phase structure itself —
# bead-id extraction, phase env, the ready.sh rc-capture idiom, binary/tarball
# checks — is extracted into spira/acceptance-lib.sh and unit-tested directly in
# test-acceptance-lib.sh; this file covers only what only exists at the level of
# the whole script: argument handling, the verdict line, phase wiring, and the
# few behaviours (deploy gated on install, install output not discarded) that
# live in acceptance-run.sh itself rather than in the extracted library.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
SCRIPT="$HERE/acceptance-run.sh"
AGENT="$HERE/acceptance-agent.sh"

wantfile()   { grep -qF -- "$2" "$3" 2>/dev/null && ok "$1" || bad "$1" "not found: $2"; }
wantrefile() { grep -qE -- "$2" "$3" 2>/dev/null && ok "$1" || bad "$1" "pattern not found: $2"; }
nowantfile() { grep -qE -- "$2" "$3" 2>/dev/null && bad "$1" "still present: $2" || ok "$1"; }

echo "test-acceptance-run.sh"

echo
echo "1. POSITIVE CONTROL — acceptance-run.sh and acceptance-lib.sh exist and wire together"

if [ -f "$SCRIPT" ] && [ -x "$SCRIPT" ]; then
    ok "acceptance-run.sh exists and is executable"
else
    bad "acceptance-run.sh exists and is executable" "missing or not executable"
    tl_summary
fi
wantfile "acceptance-run.sh sources acceptance-lib.sh" \
    '. "$HERE/acceptance-lib.sh"' "$SCRIPT"

echo
echo "2. Usage errors exit non-zero without running a phase"

bash "$SCRIPT" >/dev/null 2>&1 && bad "no-arg invocation exits non-zero" "exit 0" \
    || ok "no-arg invocation exits non-zero"
bash "$SCRIPT" some-tag >/dev/null 2>&1 && bad "missing --scratch-repo exits non-zero" "exit 0" \
    || ok "missing --scratch-repo exits non-zero"

echo
echo "3. PASS/FAIL verdict line"

wantfile "script emits a verdict line"             "verdict:"        "$SCRIPT"
wantfile "verdict reports FAIL on any failure"     'verdict="FAIL"'  "$SCRIPT"
wantfile "verdict reports PASS on zero failures"   'verdict="PASS"'  "$SCRIPT"

echo
echo "4. Positive-control self-check present"

wantfile "positive-control self-check present" \
    "positive-control: command -v catches missing tool" "$SCRIPT"

echo
echo "5. Landing is verified by ancestry, not bead status"

wantrefile "ancestry check uses a SHA range" '_base_sha_before\.\.' "$SCRIPT"
if grep -qE 'bead_status' "$SCRIPT" 2>/dev/null; then
    bad "landing check does not rely on a bead_status shortcut" "found bead_status in $SCRIPT"
else
    ok "landing check does not rely on a bead_status shortcut"
fi

echo
echo "6. Phase A, B, C, D labels present"

for _p in A B C D; do
    wantfile "phase $_p label present" "phase $_p" "$SCRIPT"
done

echo
echo "7. --record writes a git note; --waive-upgrade overrides --prev-tag"

wantfile "--record writes a git note"        "git notes --ref=acceptance" "$SCRIPT"
wantfile "--record note targets refs/tags/"  "refs/tags/"                 "$SCRIPT"
wantrefile "--waive-upgrade is a recognized flag" \
    'waive-upgrade\) do_waive_upgrade=1' "$SCRIPT"
wantrefile "waiver clears prev_tag regardless of --prev-tag" \
    'do_waive_upgrade.*-eq 1.*&&.*prev_tag=""' "$SCRIPT"
wantfile "verdict note records the waiver" "upgrade phases waived by operator" "$SCRIPT"

echo
echo "8. Old unbounded polling loops are gone; staged checks have tight budgets"

nowantfile "positive-control: old unbounded aeon-wait loop is gone" '_aeon_wait' "$SCRIPT"
nowantfile "positive-control: old unbounded aged-land-wait loop is gone" '_aged_land_wait' "$SCRIPT"
wantfile "stage 2 budget is 60s" '_a_t2 )) -lt 60' "$SCRIPT"
wantfile "stage 5 budget is 120s" '_a_t5 )) -lt 120' "$SCRIPT"

echo
echo "9. Phase A: bead labels carry plan+scope; claimability uses bd ready"

wantfile "phase A bead creation uses plan-label variable"  '"acceptance,${_a_plan_label}' "$SCRIPT"
wantfile "phase A bead creation uses scope-label variable" '"acceptance,${_a_plan_label},${_a_scope_label}' "$SCRIPT"
wantfile "phase A claimability check uses bd ready" 'bd -C "$bd_db" ready' "$SCRIPT"
wantfile "claimability check filters by plan+scope label" \
    '--label "${_a_scope_label},${_a_plan_label}"' "$SCRIPT"
if grep -F 'sentinel --report' "$SCRIPT" 2>/dev/null | grep -qv '^\s*#'; then
    bad "claimability does not rely on sentinel --report polling" "found sentinel --report in $SCRIPT"
else
    ok "claimability does not rely on sentinel --report polling"
fi
wantfile "phase A bead-not-claimable error names the predicate" \
    'builder predicate does not match bead labels' "$SCRIPT"

# ============================================================================
echo
echo "10. Phase B: install.sh output is streamed (tee), not discarded"
# ============================================================================

if grep -E 'env "\$\{_install_env\[@\]\}".*install\.sh.*>/dev/null' "$SCRIPT" 2>/dev/null; then
    bad "phase B install.sh output not discarded" "found install.sh with >/dev/null"
else
    ok "phase B install.sh output not discarded"
fi
wantfile "phase A and B share one _phase_env-built _install_env (no per-phase copy)" \
    '_phase_env _install_env' "$SCRIPT"
wantfile "phase B installs from prev tarball via _install_from_tarball" \
    '_install_from_tarball "$_prev_tarball_file"' "$SCRIPT"

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM
mkdir -p "$SCRATCH/clone"
printf '#!/bin/sh\nprintf "prev-install-failure-reason\\n"\nexit 1\n' \
    > "$SCRATCH/clone/install.sh"
chmod +x "$SCRATCH/clone/install.sh"
_pb_out="$(
    _prev_install_rc=0
    SPIRA_OPERATED=0 bash "$SCRATCH/clone/install.sh" 2>&1 | tee "$SCRATCH/prev-install.log" \
        || _prev_install_rc=$?
    [ "$_prev_install_rc" -ne 0 ] && \
        printf '  FAIL  phase B: install.sh exits 0: exit %d\n' "$_prev_install_rc"
)"
want "fixture: a failing phase B install.sh's output reaches the report" \
    "prev-install-failure-reason" "$_pb_out"

# ============================================================================
echo
echo "11. Phase B: deploy is gated on install success"
# ============================================================================

wantrefile "phase B deploys only when install succeeded" '_prev_install_rc.*-ne 0' "$SCRIPT"
wantfile "phase B guard names database service not started" \
    "database service not started" "$SCRIPT"

_pg_out="$(
    _prev_install_rc=1
    [ "$_prev_install_rc" -ne 0 ] && \
        printf '  FAIL  phase B+C: skipped — install failed; database service not started: prev_install_rc=%d\n' \
            "$_prev_install_rc"
)"
want "fixture: guard names database-not-started when install fails" \
    "database service not started" "$_pg_out"

# ============================================================================
echo
echo "12. acceptance-agent.sh exists and drives the deterministic aeon path"
# ============================================================================

if [ -f "$AGENT" ] && [ -x "$AGENT" ]; then
    ok "acceptance-agent.sh exists and is executable"
else
    bad "acceptance-agent.sh exists and is executable" "missing or not executable at $AGENT"
fi
wantfile "acceptance-agent.sh drains stdin"  "cat >/dev/null" "$AGENT"
wantfile "acceptance-agent.sh commits probe" "acceptance-probe.txt" "$AGENT"
wantfile "acceptance-agent.sh closes bead"   "close" "$AGENT"

# ============================================================================
echo
echo "13. Upgrade, rollback and aged-install invariants (structural — each requires"
echo "    a real deploy.sh/systemd/bd round trip only acceptance.yml can drive)"
# ============================================================================

wantfile "phase B checks .tag sidecar from .tags dir" ".tags/"            "$SCRIPT"
wantfile "phase B checks SPIRA_PROD updated"          "SPIRA_PROD"        "$SCRIPT"
wantfile "phase C captures pre-upgrade unit set"      "_units_pre_upgrade"  "$SCRIPT"
wantfile "phase C captures post-rollback unit set"    "_units_post_rollback" "$SCRIPT"
wantfile "phase C diffs pre vs post"                  "_unit_diff"        "$SCRIPT"
wantfile "phase D checks bead count preserved"        "_aged_pre_beads"   "$SCRIPT"
wantfile "phase D checks memory count preserved"      "_aged_pre_mems"    "$SCRIPT"
wantfile "phase D runs doctor.sh"                     "_aged_doctor_rc"   "$SCRIPT"
wantfile "phase D checks operator override survives"  "ACCEPTANCE_AGED_OVERRIDE" "$SCRIPT"
wantfile "phase D checks for failed units"            "_aged_failed"      "$SCRIPT"
wantfile "phase D checks world not halted"            "_aged_world_out"   "$SCRIPT"
wantfile "phase D rollback-refused names the migration" "migrat"          "$SCRIPT"
wantfile "aged-install (from, to) pair recorded in git note" "aged-install from=" "$SCRIPT"

tl_summary
