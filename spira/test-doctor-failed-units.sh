#!/usr/bin/env bash
#
# test-doctor-failed-units.sh — doctor.sh's doctor_check_failed_units: no failed spira-*
# systemd unit goes unreported (sp-niqjl: a unit failed for four days and nothing checked).
#
#   ./test-doctor-failed-units.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL. A stubbed systemctl reporting one failed unit must produce a FAIL
#    naming it before the clean case is trusted.
# 2. CLEAN CASE. No failed units: ok, no FAIL.
# 3. MULTIPLE FAILURES. Each failed unit gets its own FAIL line.
# 4. PROBE FAILURE. systemctl itself failing (unreachable manager) is a FAIL, never a
#    silent "no failed units" — an unanswerable probe must not read as a clean bill of
#    health.
#
# tier: T1
# covers: spira/doctor.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-doctor-failed-units.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

BIN="$TMP/bin"
mkdir -p "$BIN" "$TMP/run" "$TMP/home"

cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
exit 0
FAKESCRIPT
chmod +x "$BIN/bd"

write_systemctl() {
    # $1: what `list-units --state=failed` should print (no-legend form); "PROBE_FAIL"
    # makes systemctl itself fail as if the user manager were unreachable.
    if [ "$1" = PROBE_FAIL ]; then
        cat > "$BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"list-units"*"--state=failed"*)
        printf 'Failed to connect to bus\n' >&2; exit 1 ;;
esac
exit 0
MOCK
    else
        # REAL systemctl MARKS A FAILED UNIT WITH A LEADING "● " unless --plain is given
        # (acceptance phase B printed "● is a failed systemd unit" three times, naming
        # nothing). The mock reproduces that, so a doctor that forgets --plain is caught.
        cat > "$BIN/systemctl" <<MOCK
#!/usr/bin/env bash
case "\$*" in
    *"list-units"*"--state=failed"*)
        case " \$* " in
            *" --plain "*) printf '%s\n' "$1" ;;
            *) [ -n "$1" ] && printf '%s\n' "$1" | sed 's/^/● /' ;;
        esac ;;
esac
exit 0
MOCK
    fi
    chmod +x "$BIN/systemctl"
}

run_doctor() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_BD="$BIN/bd" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        bash "$HERE/doctor.sh" 2>/dev/null
}
units_section() { sed -n '/^systemd units$/,/^$/p' <<< "$1"; }

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — one failed unit FAILs, naming it:"
# ==========================================================================
write_systemctl "spira-czar-pass-prod.service loaded failed failed czar-pass"
out="$(run_doctor || true)"
sec="$(units_section "$out")"
want "positive control: FAIL fires" "FAIL" "$sec"
want "positive control: names the unit" "spira-czar-pass-prod.service" "$sec"
nowant "positive control: never names the status bullet as the unit" "● is a failed" "$sec"

# ==========================================================================
echo
echo "2. CLEAN CASE — no failed units: ok, no FAIL:"
# ==========================================================================
write_systemctl ""
clean_out="$(run_doctor || true)"
clean_sec="$(units_section "$clean_out")"
want   "clean: ok line" "no failed spira-* units" "$clean_sec"
nowant "clean: no FAIL" "FAIL" "$clean_sec"

# ==========================================================================
echo
echo "3. MULTIPLE FAILURES — each unit gets its own FAIL line:"
# ==========================================================================
write_systemctl "$(printf '%s\n%s' \
    'spira-czar-pass-prod.service loaded failed failed czar-pass' \
    'spira-watch-answers-prod.service loaded failed failed watch-answers')"
multi_out="$(run_doctor || true)"
multi_sec="$(units_section "$multi_out")"
want "multi: first unit named" "spira-czar-pass-prod.service" "$multi_sec"
want "multi: second unit named" "spira-watch-answers-prod.service" "$multi_sec"
[ "$(grep -c 'FAIL' <<< "$multi_sec")" -eq 2 ] \
    && ok "multi: exactly two FAIL lines" \
    || bad "multi: exactly two FAIL lines" "$(grep -c 'FAIL' <<< "$multi_sec") FAIL lines"

# ==========================================================================
echo
echo "4. PROBE FAILURE — systemctl unreachable FAILs, never reads as clean:"
# ==========================================================================
write_systemctl PROBE_FAIL
probe_out="$(run_doctor || true)"
probe_sec="$(units_section "$probe_out")"
want   "probe failure: FAIL fires" "FAIL" "$probe_sec"
nowant "probe failure: no false-clean ok" "no failed spira-* units" "$probe_sec"

echo
tl_summary
