#!/usr/bin/env bash
#
# test-doctor-events-probe.sh — doctor.sh events substrate round-trip probe (db-9dh)
#
# WHAT THIS GUARDS
# ----------------
# db-9dh replaced the dolt-on-PATH proxy in doctor.sh with a write/read round trip:
# write one sentinel event via _bump_write_event, read it back via _counter_events_query,
# FAIL naming the write path when it does not return. Without this, seven consecutive
# Maechen passes reported census=0; every call to bump_requeue/recur/reclaim had been
# silently discarded and the dolt check reported OK.
#
# Two properties are tested:
#
#   1. POSITIVE CONTROL. An embedded store where events.log is unwritable (chmod 0444)
#      must FAIL, naming the file-fallback write path. Without this the fix has nothing
#      to confirm — a probe that silently passes a broken write path is no probe at all.
#
#   2. HAPPY PATH. An embedded store with no dolt and a writable SPIRA_DB must report
#      "events write/read round trip" as OK (the file fallback introduced by db-wx4 works).
#
# The positive control was run against the unfixed tree (lines 64-72 before db-9dh):
# doctor.sh printed no "events" line at all — the probe was never performed.
#
# covers: spira/doctor.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in output"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in output" ;; *) ok "$1"; esac; }

echo "test-doctor-events-probe.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Shared fixtures
# ---------------------------------------------------------------------------
BIN="$TMP/bin"
mkdir -p "$BIN"

# bd: refuse "sql" (simulates the CGO_ENABLED=0 embedded binary), pass "list" and
# "migrate schema" so the database and schema sections of doctor.sh do not FAIL.
cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
for arg in "$@"; do
    if [ "$arg" = "sql" ]; then
        printf "Error: 'bd sql' is not yet supported in embedded mode\n" >&2
        exit 1
    fi
done
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*)           printf '[]\n'; exit 0 ;;
    *)                  exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

# systemctl: all units active/enabled so the systemd sections do not FAIL.
cat > "$BIN/systemctl" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)           printf 'active\n'; exit 0 ;;
    *"is-enabled"*)          printf 'enabled\n'; exit 0 ;;
    *"list-unit-files"*)     true ;;
    *"list-units"*)          true ;;
    *"list-timers"*)         true ;;
esac
exit 0
FAKESCRIPT
chmod +x "$BIN/systemctl"

# Build a PATH without dolt so both test cases run in the file-fallback path.
_SAFE_PATH="$BIN"
IFS=: read -ra _pathdirs <<< "${PATH:-/usr/local/bin:/usr/bin:/bin}"
for _d in "${_pathdirs[@]}"; do
    [ -n "$_d" ] || continue
    [ -x "$_d/dolt" ] && continue
    _SAFE_PATH="${_SAFE_PATH}:${_d}"
done

touch "$TMP/watchers-empty"

# run_doctor <extra_env_assignments...>
# Runs doctor.sh under env -i so no ambient spira.conf can sneak in.
run_doctor() {
    local extra="${1:-}"
    env -i \
        PATH="$_SAFE_PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_BD="$BIN/bd" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        ${extra} \
        bash "$HERE/doctor.sh" 2>/dev/null
}

# ==========================================================================
echo
echo "positive control — unwritable events.log must FAIL:"
# ==========================================================================
# Create the embedded store directories, seed events.log as unwritable so
# _bump_write_event's file-fallback silently discards the write, and the
# read-back returns 0, triggering the FAIL path.
mkdir -p "$TMP/db/.beads/embeddeddolt" "$TMP/run" "$TMP/home"
touch "$TMP/db/events.log"
chmod 0444 "$TMP/db/events.log"

ctrl_out="$(run_doctor || true)"

# Confirm doctor.sh mentioned events at all — required before trusting the passing case.
want "positive control: events section appears" "events substrate" "$ctrl_out"
# The probe must FAIL because the write was silently discarded.
want "positive control: FAIL line appears"      "FAIL"             "$ctrl_out"
# The failure message must name the file-fallback path.
want "positive control: events.log named"       "events.log"       "$ctrl_out"

# Restore write permission for cleanup and subsequent case.
chmod 0644 "$TMP/db/events.log"

# ==========================================================================
echo
echo "happy path — file fallback, no dolt: events round trip must be OK:"
# ==========================================================================
rm -rf "$TMP/db"
mkdir -p "$TMP/db/.beads/embeddeddolt" "$TMP/run" "$TMP/home"
# No events.log pre-created; _bump_write_event creates it on first write.

happy_out="$(run_doctor || true)"

want   "happy path: events round trip OK"  "events write/read round trip"  "$happy_out"
nowant "happy path: no FAIL for events"    "FAIL" \
       "$(printf '%s\n' "$happy_out" | grep -i 'events' || true)"
# The section header must appear regardless of outcome.
want   "happy path: section header present" "events substrate" "$happy_out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
