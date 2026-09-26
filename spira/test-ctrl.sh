#!/usr/bin/env bash
#
# test-ctrl.sh — ctrl.sh: operational control plane outside source control.
#
#   ./test-ctrl.sh
#
# WHAT THIS SUITE COVERS
# ----------------------
# Two tiers:
#
# 1. ctrl.sh core: suspend/resume/check/list cycle; rejection of incomplete entries.
#
# 2. divergence detection: ctrl.sh divergence reports a suspended unit that is actually
#    active/enabled in systemd, and is silent when no mismatch exists.
#    Positive control: remove the planted suspension → silence, proving silence is
#    agreement and not a broken check (law-absence-needs-a-positive-control).
#
# install.sh's own honouring of a suspension is table-driven in test-install-decide.sh
# over the shared (suspended?, enabled?) predicate — cluster 6, docs/test-plan/
# instance-lifecycle.md — not re-driven here through a full install.sh run.
#
# systemctl IS STUBBED throughout. A suite that queries the real service manager is
# green for as long as the box happens to be in the state its author had.
#
# tier: T1
# covers: spira/ctrl.sh UC-instance-lifecycle-45
# hermetic-ok: SPIRA_CTRL env pins control file; SPIRA_SYSTEMCTL stubs systemctl
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
rc_is() { wantrc "$1" "$2" "$3"; }

echo "test-ctrl.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
CTRL_FILE="$TMP/control"

# ctrl.sh needs conf.sh. Run it with a minimal env pointing SPIRA_CTRL at our temp file
# and SPIRA_CONF=/nonexistent so no real config is loaded.
# SPIRA_SYSTEMCTL is set to the stub for divergence tests.
ctrl() {
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_CTRL="$CTRL_FILE" \
        SPIRA_INSTANCE=prod \
        SPIRA_SYSTEMCTL="$TMP/sc" \
        bash "$HERE/ctrl.sh" "$@"
}

# write_sc <active_unit> <enabled_unit> [masked_units...]
# Write a systemctl stub that bakes the values in so no env vars are needed at runtime.
# The divergence command passes SPIRA_SYSTEMCTL=$TMP/sc, which is this stub.
# masked_units is a space-separated list of unit names that list-unit-files should report
# as masked; used by the undeclared-direction divergence tests.
write_sc() {
    local active_unit="${1:-}" enabled_unit="${2:-}" masked_units="${3:-}"
    cat > "$TMP/sc" <<SC
#!/usr/bin/env bash
masked_units="${masked_units}"
verb=""; subject=""
for a; do
    case "\$a" in --user|--no-legend|--no-pager) ;; *) [ -z "\$verb" ] && verb="\$a" || subject="\$a" ;; esac
done
case "\$verb" in
    is-active)
        [ "\$subject" = "${active_unit}" ] && { echo active; exit 0; }
        echo inactive; exit 3 ;;
    is-enabled)
        [ "\$subject" = "${enabled_unit}" ] && { echo enabled; exit 0; }
        for mu in \$masked_units; do
            [ "\$subject" = "\$mu" ] && { echo masked; exit 1; }
        done
        echo disabled; exit 1 ;;
    list-unit-files)
        for mu in \$masked_units; do
            printf '%s  masked  enabled\n' "\$mu"
        done ;;
    *)  exit 0 ;;
esac
SC
    chmod +x "$TMP/sc"
}
write_sc "" ""   # initial stub: nothing is active, enabled, or masked

mkdir -p "$TMP/run" "$TMP/home/.config/systemd/user"

# ==========================================================================
echo
echo "CORE: suspend / check / list / resume cycle"
# ==========================================================================

# Without --reason should fail
out="$(ctrl suspend test-unit --owner sp-x000 2>&1)"; rc=$?
rc_is  "suspend without --reason exits non-zero" 1 $rc
want   "suspend without --reason prints error" "--reason" "$out"

# Without --owner should fail
out="$(ctrl suspend test-unit --reason "testing" 2>&1)"; rc=$?
rc_is  "suspend without --owner exits non-zero" 1 $rc
want   "suspend without --owner prints error" "--owner" "$out"

# check before any entry → exits 1
ctrl check test-unit >/dev/null 2>&1; rc=$?
rc_is "check before suspend exits 1" 1 $rc

# Successful suspend
out="$(ctrl suspend test-unit --reason "unit under test" --owner sp-x000 2>&1)"; rc=$?
rc_is "suspend succeeds" 0 $rc
want  "suspend output names the subject" "test-unit" "$out"
want  "suspend output names the owner"   "sp-x000"   "$out"

# check after suspend → exits 0
ctrl check test-unit >/dev/null 2>&1; rc=$?
rc_is "check after suspend exits 0" 0 $rc

# check on a different subject → exits 1 (only test-unit is suspended)
ctrl check other-unit >/dev/null 2>&1; rc=$?
rc_is "check on unsuspended subject exits 1" 1 $rc

# list shows the entry
out="$(ctrl list 2>&1)"; rc=$?
rc_is "list exits 0" 0 $rc
want  "list shows subject"    "test-unit"       "$out"
want  "list shows op"         "[suspend]"        "$out"
want  "list shows owner"      "sp-x000"          "$out"
want  "list shows reason"     "unit under test"  "$out"

# resume
out="$(ctrl resume test-unit 2>&1)"; rc=$?
rc_is "resume exits 0" 0 $rc
want  "resume output names the subject" "test-unit" "$out"

# check after resume → exits 1
ctrl check test-unit >/dev/null 2>&1; rc=$?
rc_is "check after resume exits 1" 1 $rc

# list after resume → empty
out="$(ctrl list 2>&1)"
nowant "list after resume does not show old entry" "test-unit" "$out"

# resume on non-suspended subject → graceful, no error
out="$(ctrl resume nonexistent 2>&1)"; rc=$?
rc_is "resume of non-suspended subject exits 0" 0 $rc
want  "resume of non-suspended subject says so" "not suspended" "$out"

# ==========================================================================
echo
echo "DIVERGENCE: suspended-but-running detection"
# ==========================================================================

# No control file → exits 0, both directions find nothing.
rm -f "$CTRL_FILE"
write_sc "" ""
out="$(ctrl divergence 2>&1)"; rc=$?
rc_is "divergence with no ctrl file exits 0" 0 $rc
want  "divergence with no ctrl file reports summary" "0 divergences" "$out"

# Suspend a unit; systemctl says it's inactive → no divergence
ctrl suspend spira-suites --reason "test" --owner sp-x000 >/dev/null
write_sc "" ""   # nothing active or enabled
out="$(ctrl divergence 2>&1)"; rc=$?
rc_is "divergence: suspended + inactive exits 0" 0 $rc
nowant "divergence: suspended + inactive is silent" "DIVERGENCE" "$out"

# POSITIVE CONTROL: prove the divergence check ran by planting a divergence.
# Stub says the suspended unit's timer is active — this must be caught.
write_sc "spira-suites-prod.timer" ""
out="$(ctrl divergence 2>&1)"; rc=$?
rc_is "divergence: suspended + active exits 1" 1 $rc
want  "divergence: names the subject" "spira-suites" "$out"
want  "divergence: prints DIVERGENCE" "DIVERGENCE"   "$out"

# Also catches 'enabled' (timer enabled but not yet ticking)
write_sc "" "spira-suites-prod.timer"
out="$(ctrl divergence 2>&1)"; rc=$?
rc_is "divergence: suspended + enabled exits 1" 1 $rc
want  "divergence: catches enabled unit" "DIVERGENCE" "$out"

# Positive control: remove the suspension → divergence check runs but finds nothing.
# This proves silence means agreement, not a broken check.
ctrl resume spira-suites >/dev/null
write_sc "spira-suites-prod.timer" ""   # unit is still "active" in stub
out="$(ctrl divergence 2>&1)"; rc=$?
rc_is "divergence: no suspension → exits 0 even when unit is active" 0 $rc
nowant "divergence: without suspension, active unit is not a divergence" "DIVERGENCE" "$out"

# ==========================================================================
echo
echo "DIVERGENCE: undeclared direction (masked without control-plane entry)"
# ==========================================================================

# Negative control: no masked units, no entries — divergence exits 0 (baseline proves it runs).
rm -f "$CTRL_FILE"
write_sc "" "" ""
out="$(ctrl divergence 2>&1)"; rc=$?
rc_is "undeclared: no masked units, no entries → exits 0" 0 $rc
nowant "undeclared: no masked units → no DIVERGENCE" "DIVERGENCE" "$out"

# Plant the offender: a unit is masked in systemd with NO control-plane entry.
# This is the shape found in production — units masked outside the control plane.
# The check must exit non-zero.
write_sc "" "" "spira-suites-prod.timer"
out="$(ctrl divergence 2>&1)"; rc=$?
rc_is "undeclared: masked unit without entry exits 1" 1 $rc
want  "undeclared: prints DIVERGENCE"      "DIVERGENCE"              "$out"
want  "undeclared: names the masked unit"  "spira-suites-prod.timer" "$out"

# Positive control: register the suspension in the control plane.
# The masked unit is now declared — divergence must exit 0.
ctrl suspend spira-suites --reason "probe test" --owner sp-x000 >/dev/null
out="$(ctrl divergence 2>&1)"; rc=$?
rc_is "undeclared: declared masked unit exits 0" 0 $rc
nowant "undeclared: declared masked unit not a divergence" "DIVERGENCE" "$out"

ctrl resume spira-suites >/dev/null

# ==========================================================================
tl_summary
