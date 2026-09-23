#!/usr/bin/env bash
#
# test-ctrl.sh — ctrl.sh: operational control plane outside source control.
#
#   ./test-ctrl.sh
#
# WHAT THIS SUITE COVERS
# ----------------------
# Three tiers:
#
# 1. ctrl.sh core: suspend/resume/check/list cycle; rejection of incomplete entries.
#
# 2. divergence detection: ctrl.sh divergence reports a suspended unit that is actually
#    active/enabled in systemd, and is silent when no mismatch exists.
#    Positive control: remove the planted suspension → silence, proving silence is
#    agreement and not a broken check (law-absence-needs-a-positive-control).
#
# 3. install.sh integration: a suspended unit is skipped and the skip is named; a
#    non-suspended unit is enabled normally. Both paths are driven through the real
#    install.sh with a systemctl stub that records every call it receives.
#
# systemctl IS STUBBED throughout. A suite that queries the real service manager is
# green for as long as the box happens to be in the state its author had.
#
# covers: spira/ctrl.sh systemd/install.sh
# hermetic-ok: SPIRA_CTRL env pins control file; SPIRA_SYSTEMCTL stubs systemctl
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
REAL_COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
rc_is()  { [ "$2" -eq "$3" ] && ok "$1" || bad "$1" "wanted rc=$2 got rc=$3"; }

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
echo
echo "INSTALL.SH: suspended unit is skipped; non-suspended unit is enabled"
# ==========================================================================

# Build a minimal install.sh fixture (mirrors test-install-halt.sh).
FIXTURE="$TMP/harness"
mkdir -p "$FIXTURE/systemd" "$FIXTURE/spira"

for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer; do
    [ -e "$f" ] || continue
    ln -sf "$f" "$FIXTURE/systemd/$(basename "$f")"
done
ln -sf "$HERE/../systemd/install.sh" "$FIXTURE/systemd/install.sh"
for f in conf.sh watchd.sh lib.sh ctrl.sh; do
    [ -e "$HERE/$f" ] && ln -sf "$HERE/$f" "$FIXTURE/spira/$f"
done
printf '# empty — test fixture\n' > "$FIXTURE/spira/watchers"
printf '# empty\n' > "$FIXTURE/spira/repo-map.example"
printf '#!/usr/bin/env bash\nexit 0\n' > "$FIXTURE/spira/install-session-hook.sh"
chmod +x "$FIXTURE/spira/install-session-hook.sh"

DEST="$TMP/home/.config/systemd/user"
SPIRA_RUN_INST="$TMP/inst-run"
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$DEST" "$SPIRA_RUN_INST" "$MOCK_BIN"
MOCK_LOG="$TMP/inst-sc.log"
INST_CTRL="$TMP/inst-ctrl"

# systemctl stub: record every call. Uses ${MOCK_LOG} which is passed via env.
cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${MOCK_LOG}"
case "$*" in *is-active*) echo "inactive" ;; *is-enabled*) echo "disabled" ;; esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"

printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/loginctl"
chmod +x "$MOCK_BIN/loginctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/spira-supervise"
chmod +x "$MOCK_BIN/spira-supervise"

# SPIRA_PATH is used rather than prepending PATH directly: conf.sh resets PATH, and a
# prepend to the outer PATH is silently overwritten (law-gates-run-in-a-clean-environment).
# SPIRA_HOME is set to HERE (the real spira/ directory) so ctrl.sh is found at $SPIRA_HOME/ctrl.sh.
inst() {
    > "$MOCK_LOG"
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$MOCK_BIN" \
        SPIRA_WATCHERS="$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_RUN="$SPIRA_RUN_INST" \
        SPIRA_CTRL="$INST_CTRL" \
        SPIRA_HOME="$HERE" \
        SPIRA_PROD="$HERE" \
        SPIRA_REPO="$REAL_REPO" \
        SPIRA_COCKPIT="$REAL_COCKPIT" \
        SPIRA_INSTANCE=prod \
        "SPIRA_SUPERVISE_BIN=$MOCK_BIN/spira-supervise" \
        MOCK_LOG="$MOCK_LOG" \
        SPIRA_INSTALL_FORCE=1 \
        bash "$FIXTURE/systemd/install.sh" "$@" 2>&1
}

# Populate DEST so install.sh doesn't fail on missing rendered files.
rendered="$(inst --render 2>&1)"; rc=$?
if [ "$rc" != 0 ]; then
    printf 'FAIL fixture: install.sh --render failed (rc=%s) — cannot continue\n' "$rc"
    printf '%s\n' "$rendered" | head -20
    fail=$((fail+1))
else
    current_unit=""
    while IFS= read -r line; do
        if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
            current_unit="${BASH_REMATCH[1]}"; > "$DEST/$current_unit"
        elif [ -n "$current_unit" ]; then
            printf '%s\n' "$line" >> "$DEST/$current_unit"
        fi
    done <<< "$rendered"

    # ---- no suspension: install.sh should try to enable the suites timer
    rm -f "$INST_CTRL"
    out="$(inst 2>&1)"; log="$(cat "$MOCK_LOG")"
    want   "install: no suspension — systemctl called"         "enable"       "$log"
    nowant "install: no suspension — no skip message"          "suspended"    "$out"

    # ---- suspend spira-suites in the control file
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="$SPIRA_RUN_INST" \
        SPIRA_CTRL="$INST_CTRL" \
        SPIRA_INSTANCE=prod \
        bash "$HERE/ctrl.sh" suspend spira-suites \
            --reason "suite-red backlog" --owner sp-jxia >/dev/null

    out="$(inst 2>&1)"; log="$(cat "$MOCK_LOG")"
    want   "install: suspended — skip message printed"         "suspended"         "$out"
    want   "install: suspended — names subject in message"     "spira-suites"      "$out"
    # The timer must NOT appear in systemctl enable calls
    nowant "install: suspended — suites timer not enabled"     "spira-suites-prod.timer" "$log"

    # ---- resume the suspension: install.sh should enable again
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="$SPIRA_RUN_INST" \
        SPIRA_CTRL="$INST_CTRL" \
        SPIRA_INSTANCE=prod \
        bash "$HERE/ctrl.sh" resume spira-suites >/dev/null

    out="$(inst 2>&1)"; log="$(cat "$MOCK_LOG")"
    nowant "install: after resume — no skip message"           "suspended"    "$out"
fi

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
