#!/usr/bin/env bash
#
# test-doctor-snap-fresh.sh — doctor.sh warns when the cockpit snapshot is absent
#   or stale, and reports ok when it is fresh.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): verify the absent-
# snapshot WARN fires before trusting the fresh-snapshot OK.
#
# No database required; the check is purely filesystem-based.
#
# tier: T1
# covers: spira/doctor.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-doctor-snap-fresh.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

mkdir -p "$TMP/bin" "$TMP/db/.beads" "$TMP/run" "$TMP/home"

# Minimal fake bd: passes the checks doctor.sh runs against the database.
cat > "$TMP/bin/bd" <<'FAKEBD'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*)         printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--json"*)          printf '[{"id":"sp-test"}]\n'; exit 0 ;;
    *"show"*"sp-test"*)         printf '[{"id":"sp-test"}]\n'; exit 0 ;;
    *)                          exit 0 ;;
esac
FAKEBD
chmod +x "$TMP/bin/bd"

# Executable notify script so that check does not FAIL before reaching snapshot section.
cat > "$TMP/bin/fake-notify" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$TMP/bin/fake-notify"

# Minimal fake systemctl for the enabled-units and session-hook checks.
cat > "$TMP/bin/systemctl" <<'SH'
#!/usr/bin/env bash
printf 'enabled\n'; exit 0
SH
chmod +x "$TMP/bin/systemctl"

run_doctor() {
    env -i \
        PATH="$TMP/bin:/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$TMP/bin" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_BD="$TMP/bin/bd" \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY="$TMP/bin/fake-notify" \
        SPIRA_SYSTEMCTL="$TMP/bin/systemctl" \
        SPIRA_GOAL=sp-test \
        SPIRA_COCKPIT="$TMP/run" \
        SPIRA_SNAP_STALE_S=60 \
        "$@" \
        bash "$HERE/doctor.sh" 2>/dev/null || true
}

# ==========================================================================
echo
echo "positive control — absent snapshot fires a WARN (seen before clean case):"
# ==========================================================================

rm -f "$TMP/run/cockpit.env"
pc_out="$(run_doctor)"
want "absent snapshot: cockpit section runs"          "the cockpit"          "$pc_out"
want "absent snapshot: WARN fires"                    "warn  no cockpit snapshot" "$pc_out"
nowant "absent snapshot: no fresh OK"                 "ok    cockpit snapshot fresh" "$pc_out"

# ==========================================================================
echo
echo "stale snapshot — FAIL names the age:"
# ==========================================================================

printf 'SP_AT=0\n' > "$TMP/run/cockpit.env"
touch -d "300 seconds ago" "$TMP/run/cockpit.env"
stale_out="$(run_doctor)"
want "stale snapshot: FAIL for stale"                 "FAIL  cockpit snapshot stale" "$stale_out"
want "stale snapshot: age shown"                      "s ago"                         "$stale_out"
nowant "stale snapshot: no fresh OK"                  "ok    cockpit snapshot fresh"  "$stale_out"

# ==========================================================================
echo
echo "fresh snapshot — OK line and no stale WARN:"
# ==========================================================================

printf 'SP_AT=0\n' > "$TMP/run/cockpit.env"
fresh_out="$(run_doctor)"
want "fresh snapshot: OK line present"                "ok    cockpit snapshot fresh"  "$fresh_out"
nowant "fresh snapshot: no absent WARN"               "warn  no cockpit snapshot"     "$fresh_out"
nowant "fresh snapshot: no stale FAIL"                "FAIL  cockpit snapshot stale"  "$fresh_out"

# ==========================================================================
echo
tl_summary
