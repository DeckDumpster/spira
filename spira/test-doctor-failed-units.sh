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
# The tree's tools for a minimal-PATH run (sp-gypjk): where this suite's PATH finds the
# tree's build, and the tree's own spira/.
TOOLS="$(dirname "$(command -v spira-config)"):$HERE"

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
    # $1, if "installing", sets SPIRA_DOCTOR_INSTALLING=1 — install.sh's own phase-0
    # preflight, where a failed unit necessarily predates this run (sp-r15cf).
    local installing=""
    [ "${1:-}" = installing ] && installing=1
    env -i \
        PATH="$TOOLS:/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_BD="$BIN/bd" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        ${installing:+SPIRA_DOCTOR_INSTALLING=1} \
        doctor.sh 2>/dev/null
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

# ==========================================================================
echo
echo "5. SPIRA_DOCTOR_INSTALLING — a failed unit at preflight predates this run, so it is a"
echo "   WARN, not a FAIL: install.sh's phase 0 has not touched a single unit yet, so"
echo "   whatever doctor.sh finds failed here cannot be something THIS install broke"
echo "   (sp-r15cf: an aged install refused itself over a failure the box already had):"
# ==========================================================================
write_systemctl "spira-summon-prod.service loaded failed failed summon"
installing_out="$(run_doctor installing || true)"
installing_rc=$?
installing_sec="$(units_section "$installing_out")"
want   "installing: WARN, names the unit" "spira-summon-prod.service" "$installing_sec"
want   "installing: says pre-existing" "predates this install" "$installing_sec"
nowant "installing: no FAIL for it" "FAIL  spira-summon-prod.service" "$installing_sec"
is     "installing: doctor.sh itself does not refuse on this alone" "0" "$installing_rc"
# The ordinary (non-installing) case must be untouched by this — still a hard FAIL.
not_installing_out="$(run_doctor || true)"
not_installing_sec="$(units_section "$not_installing_out")"
want "ordinary run: still FAILs (no regression)" "FAIL  spira-summon-prod.service" "$not_installing_sec"

echo
tl_summary
