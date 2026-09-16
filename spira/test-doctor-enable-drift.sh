#!/usr/bin/env bash
#
# test-doctor-enable-drift.sh — doctor.sh's "enabled units" section FAILs when a
# member of the ENABLE set is disabled without a control-plane suspension entry,
# and passes when every member is enabled or has one.
#
#   ./test-doctor-enable-drift.sh
#
# WHAT THIS TESTS
# ---------------
# units.sh declares the ENABLE set — timers and services that must be running.
# Nothing previously checked whether they were actually enabled; a unit could be
# disabled at install time and nothing would alert. This suite verifies that:
#
#   1. POSITIVE CONTROL. A disabled unit is reported before the clean case is
#      believed. A checker that never fires looks identical to one that passes.
#
#   2. DRIFT CASE. Units that are disabled and have no ctrl.sh suspension entry
#      each produce a FAIL line naming the unit.
#
#   3. SUSPENSION EXEMPT. A unit disabled WITH a ctrl.sh entry (deliberately
#      stood down) is not reported — the check reads the control plane, not just
#      counting disabled units.
#
#   4. CLEAN CASE. When every ENABLE unit reports "enabled", the section prints
#      its OK line and no FAIL.
#
#   5. FAIL-CLOSED. If systemctl produces no output at all for a unit, the result
#      is a FAIL rather than a silent clean pass (law-alerts-must-be-actionable).
#
# covers: spira/doctor.sh systemd/units.sh spira/ctrl.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-doctor-enable-drift.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

BIN="$TMP/bin"
mkdir -p "$BIN" "$TMP/db/.beads" "$TMP/run"
# An empty regular file: watchd_rows requires test -f to pass; /dev/null is a char device.
touch "$TMP/watchers-empty"

# Fake bd: answers the calls doctor.sh makes without touching a real database.
cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

# FAKE_HOME: doctor.sh reads $HOME/.config/systemd/user for installed-units checks.
# Redirect HOME so the test never touches the real unit directory.
FAKE_HOME="$TMP/home"
mkdir -p "$FAKE_HOME/.config/systemd/user"

# write_sc <path> <disabled-pattern>
# Write a fake systemctl that returns "enabled" for is-enabled calls except when the unit
# name matches <disabled-pattern>, in which case it returns "disabled". Pass the empty
# string for <disabled-pattern> to make all units return "enabled". Pass "SILENT" to make
# is-enabled produce no output at all (the fail-closed test).
write_sc() {
    local path="$1" disabled_pat="${2:-}"
    cat > "$path" <<MOCK
#!/usr/bin/env bash
case "\$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)           printf 'active\n' ;;
    *"list-unit-files"*)     true ;;
    *"list-units"*)          true ;;
    *"list-timers"*)         true ;;
    *"is-enabled"*)
        case "\$*" in
MOCK
    if [ "$disabled_pat" = "SILENT" ]; then
        # Produce no output — the fail-closed test.
        printf '            *) exit 0 ;;\n' >> "$path"
    elif [ "$disabled_pat" = "NOTFOUND" ]; then
        # Return "not-found" for every unit — simulates a fresh install with no unit files.
        printf '            *) printf "not-found\\n"; exit 1 ;;\n' >> "$path"
    elif [ -n "$disabled_pat" ]; then
        printf '            *"%s"*) printf "disabled\\n"; exit 1 ;;\n' \
            "$disabled_pat" >> "$path"
        printf '            *) printf "enabled\\n"; exit 0 ;;\n' >> "$path"
    else
        printf '            *) printf "enabled\\n"; exit 0 ;;\n' >> "$path"
    fi
    cat >> "$path" <<'MOCK'
        esac
        ;;
esac
exit 0
MOCK
    chmod +x "$path"
}

# run_doctor <sc-path> — run doctor.sh with the given fake systemctl.
# SPIRA_CTRL defaults to $SPIRA_RUN/control, so the suspension test just writes
# that file before calling this function.
run_doctor() {
    local sc="${1:-$BIN/sc-enabled}"
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$sc" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        bash "$HERE/doctor.sh" 2>/dev/null || true
}

# ===========================================================================
echo
echo "positive control — disabled unit reported before clean case is trusted:"
# ===========================================================================
# Without this, silence and a broken probe are indistinguishable.
write_sc "$BIN/sc-drift" "spira-ops-prod.timer"
pos_out="$(run_doctor "$BIN/sc-drift")"
want "positive control: FAIL for disabled unit"  "  FAIL  spira-ops-prod.timer" "$pos_out"
want "positive control: unit name in FAIL line"  "spira-ops-prod.timer" "$pos_out"

# ===========================================================================
echo
echo "drift case — four disabled units each produce a FAIL:"
# ===========================================================================
# The four timers from the bead's evidence that were disabled with no ctrl entry.
# A single fake that rejects all four with one pattern (matching -prod.timer) would
# pass even if individual unit names were wrong; write one that matches each by name
# so the test is specific about which units the check must name.
cat > "$BIN/sc-four-disabled" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)           printf 'active\n' ;;
    *"list-unit-files"*)     true ;;
    *"list-units"*)          true ;;
    *"list-timers"*)         true ;;
    *"is-enabled"*)
        case "$*" in
            *"spira-ops-prod.timer"*)      printf 'disabled\n'; exit 1 ;;
            *"spira-promote-prod.timer"*)  printf 'disabled\n'; exit 1 ;;
            *"spira-suites-prod.timer"*)   printf 'disabled\n'; exit 1 ;;
            *"spira-watchtower-prod.timer"*) printf 'disabled\n'; exit 1 ;;
            *) printf 'enabled\n'; exit 0 ;;
        esac
        ;;
esac
exit 0
MOCK
chmod +x "$BIN/sc-four-disabled"

four_out="$(run_doctor "$BIN/sc-four-disabled")"
want "drift: ops timer reported"        "spira-ops-prod.timer"        "$four_out"
want "drift: promote timer reported"    "spira-promote-prod.timer"    "$four_out"
want "drift: suites timer reported"     "spira-suites-prod.timer"     "$four_out"
want "drift: watchtower timer reported" "spira-watchtower-prod.timer" "$four_out"
# Each must produce a FAIL line (not just appear in a hint line).
want "drift: ops FAIL line"        "  FAIL  spira-ops-prod.timer"        "$four_out"
want "drift: promote FAIL line"    "  FAIL  spira-promote-prod.timer"    "$four_out"
want "drift: suites FAIL line"     "  FAIL  spira-suites-prod.timer"     "$four_out"
want "drift: watchtower FAIL line" "  FAIL  spira-watchtower-prod.timer" "$four_out"
nowant "drift: no OK line when units are broken" \
    "ENABLE units are enabled" "$four_out"

# Count matches for the four-timer pattern (acceptance-criteria check).
drift_count="$(printf '%s\n' "$four_out" \
    | grep -cE 'spira-(ops|promote|suites|watchtower)-prod\.timer' || true)"
[ "$drift_count" -ge 4 ] \
    && ok "drift: at least 4 lines name the four disabled timers (count=$drift_count)" \
    || bad "drift: expected >=4 lines naming the four timers, got $drift_count"

# ===========================================================================
echo
echo "suspension exempt — suspended unit drops from report; others remain:"
# ===========================================================================
# Write a ctrl.sh control file suspending spira-suites (the subject, without instance
# suffix or extension). After this, the check must report the other three but not suites.
mkdir -p "$TMP/run"
cat > "$TMP/run/control" <<'JSON'
{
  "spira-suites": {
    "suspend": {
      "reason": "test suspension",
      "owner": "db-eqo",
      "when": "2026-09-14",
      "by": "test"
    }
  }
}
JSON

susp_out="$(run_doctor "$BIN/sc-four-disabled")"
want   "suspension: ops still reported"        "spira-ops-prod.timer"        "$susp_out"
want   "suspension: promote still reported"    "spira-promote-prod.timer"    "$susp_out"
want   "suspension: watchtower still reported" "spira-watchtower-prod.timer" "$susp_out"
nowant "suspension: suites NOT reported (it is suspended)" \
    "  FAIL  spira-suites-prod.timer" "$susp_out"

# Remove the control file for subsequent tests.
rm -f "$TMP/run/control"

# ===========================================================================
echo
echo "clean case — all units enabled: OK line printed, no FAIL:"
# ===========================================================================
write_sc "$BIN/sc-enabled" ""
clean_out="$(run_doctor "$BIN/sc-enabled")"
want   "clean: OK line present"   "ENABLE units are enabled" "$clean_out"
nowant "clean: no FAIL line"      "  FAIL  " \
    "$(printf '%s\n' "$clean_out" | grep 'enabled units' -A 100 || true)"

# ===========================================================================
echo
echo "fail-closed — systemctl returning no output is a FAIL, not a clean pass:"
# ===========================================================================
# A probe that says clean when it cannot see the state is the silence that hides
# the outage (law-alerts-must-be-actionable).
write_sc "$BIN/sc-silent" "SILENT"
silent_out="$(run_doctor "$BIN/sc-silent")"
want   "fail-closed: FAIL when systemctl returns nothing" \
    "  FAIL  " "$silent_out"
want   "fail-closed: error message mentions enablement state" \
    "cannot verify enablement state" "$silent_out"
nowant "fail-closed: no OK for enabled units" \
    "ENABLE units are enabled" "$silent_out"

# ===========================================================================
echo
echo "not-found — fresh install, no unit files installed: must not fail preflight:"
# ===========================================================================
# On a fresh host, units have not been installed yet. systemctl returns
# "not-found" for every unit. This is not drift — the unit simply has not
# been installed. The bead that prompted this: sp-jcb1.
write_sc "$BIN/sc-notfound" "NOTFOUND"
notfound_out="$(run_doctor "$BIN/sc-notfound")"
nowant "not-found: no FAIL in enabled-units section" "  FAIL  " \
    "$(printf '%s\n' "$notfound_out" | grep -A 100 'enabled units' || true)"
want "not-found: OK line present" "ENABLE units are enabled" "$notfound_out"

# ===========================================================================
echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
