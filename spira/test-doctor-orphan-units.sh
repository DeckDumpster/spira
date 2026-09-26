#!/usr/bin/env bash
#
# test-doctor-orphan-units.sh — doctor.sh's doctor_check_orphan_units: an enabled watch unit
# whose watcher has no `daemon` row in the manifest goes FAIL (sp-07yxy:
# spira-watch-answers-prod.service stayed enabled and failed after "answers" was retired,
# and nothing checked for it).
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL. An enabled unit whose watcher name has no daemon row FAILs, naming
#    both the unit and the watcher. This proves the check can detect the defect before the
#    clean case is trusted.
# 2. CLEAN CASE. Every enabled watch unit's name has a daemon row: ok, no FAIL.
# 3. RETIRED TO A NON-DAEMON KIND (e.g. `log`) still orphans the old unit — a row that
#    changed kind is not the same as a row still owning that unit.
# 4. PROBE FAILURE. systemctl itself failing (unreachable manager) is a FAIL, never a
#    silent "no orphan units" — an unanswerable probe must not read as a clean bill of
#    health.
#
# tier: T1
# covers: spira/doctor.sh spira/watchd.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-doctor-orphan-units.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

BIN="$TMP/bin"
mkdir -p "$BIN" "$TMP/run" "$TMP/home"

cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
exit 0
FAKESCRIPT
chmod +x "$BIN/bd"

# The manifest doctor_check_orphan_units reads through watchd.sh manifest — a scratch file,
# never the real installation's, per SPIRA_WATCHERS below.
MAN="$TMP/watchers"

write_systemctl() {
    # $1: the one line `list-unit-files --state=enabled` should print (no-legend form,
    # "<unit> enabled"); "PROBE_FAIL" makes systemctl itself fail as if the user manager
    # were unreachable.
    if [ "$1" = PROBE_FAIL ]; then
        cat > "$BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"list-unit-files"*"--state=enabled"*)
        printf 'Failed to connect to bus\n' >&2; exit 1 ;;
esac
exit 0
MOCK
    else
        cat > "$BIN/systemctl" <<MOCK
#!/usr/bin/env bash
case "\$*" in
    *"list-unit-files"*"--state=enabled"*)
        [ -n "$1" ] && printf '%s\n' "$1" ;;
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
        SPIRA_WATCHERS="$MAN" \
        bash "$HERE/doctor.sh" 2>/dev/null
}
units_section() { sed -n '/^systemd units$/,/^$/p' <<< "$1"; }

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — enabled unit with no daemon row FAILs, naming it:"
# ==========================================================================
printf 'answers|daemon|/usr/bin/true\n' > "$MAN"
write_systemctl "spira-watch-retired-prod.service enabled"
out="$(run_doctor || true)"
sec="$(units_section "$out")"
want "positive control: FAIL fires"       "FAIL"                                "$sec"
want "positive control: names the unit"   "spira-watch-retired-prod.service"    "$sec"
want "positive control: names the watcher" "'retired'"                          "$sec"

# ==========================================================================
echo
echo "2. CLEAN CASE — every enabled unit's watcher has a daemon row: ok, no FAIL:"
# ==========================================================================
printf 'answers|daemon|/usr/bin/true\n' > "$MAN"
write_systemctl "spira-watch-answers-prod.service enabled"
clean_out="$(run_doctor || true)"
clean_sec="$(units_section "$clean_out")"
want   "clean: ok line" "no orphan spira-watch units" "$clean_sec"
nowant "clean: no FAIL naming an orphan" "has no daemon row" "$clean_sec"

# ==========================================================================
echo
echo "3. RETIRED TO A NON-DAEMON KIND still orphans the old unit:"
# ==========================================================================
printf 'answers|log|/var/log/answers.log\n' > "$MAN"
write_systemctl "spira-watch-answers-prod.service enabled"
demoted_out="$(run_doctor || true)"
demoted_sec="$(units_section "$demoted_out")"
want "demoted: FAIL fires"     "FAIL"                                "$demoted_sec"
want "demoted: names the unit" "spira-watch-answers-prod.service"    "$demoted_sec"

# ==========================================================================
echo
echo "4. PROBE FAILURE — systemctl unreachable FAILs, never reads as clean:"
# ==========================================================================
printf 'answers|daemon|/usr/bin/true\n' > "$MAN"
write_systemctl PROBE_FAIL
probe_out="$(run_doctor || true)"
probe_sec="$(units_section "$probe_out")"
want   "probe failure: FAIL fires"       "FAIL"                          "$probe_sec"
nowant "probe failure: no false-clean ok" "no orphan spira-watch units" "$probe_sec"

echo
tl_summary
