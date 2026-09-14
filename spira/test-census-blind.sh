#!/usr/bin/env bash
#
# test-census-blind.sh — census.sh and doctor.sh fail closed when the events
# substrate is unreachable (embedded store, dolt absent from PATH).
#
#   ./test-census-blind.sh
#
# WHAT THIS GUARDS
# ----------------
# Before this fix, census_events_run_sql returned exit 0 with empty output when both
# access paths (bd sql, dolt) were unavailable. census.sh consumed that silence as
# "zero classes ranked" — identical output to a genuinely clean corpus. Six Maechen
# passes completed against a blind census and none recorded an outcome.
#
# doctor.sh placed dolt in the WARN loop; on an embedded store the absence of dolt
# is not a degraded feature but a dead subsystem, so the severity was wrong.
#
# THREE ACCEPTANCE CRITERIA:
#   1. POSITIVE CONTROL. With dolt on PATH, census.sh exits 0 and reports seeded
#      events. This is required before trusting the "unreachable" result: without it,
#      a broken environment and an unreachable substrate produce the same non-zero exit.
#   2. UNREACHABLE. With dolt stripped from PATH (embedded store; bd sql is refused in
#      embedded mode), census.sh exits non-zero and writes a diagnostic to stderr.
#   3. DOCTOR. doctor.sh reports dolt as FAIL (not WARN) when the embeddeddolt
#      directory exists and dolt is not on PATH.
#
# REQUIRES dolt ON PATH. The positive control reads events from the embedded store via
# dolt. Without it the seeded events cannot be read and the positive control cannot
# establish that a working substrate produces exit 0. Skip with exit 77 when absent.
#
# covers: spira/census.sh spira/lib.sh spira/doctor.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

# Require dolt: the positive control reads events via dolt and cannot be satisfied by
# any fallback. Without it the test cannot distinguish "clean corpus" from "blind census".
if ! command -v dolt >/dev/null 2>&1; then
    printf 'SKIP test-census-blind: dolt not on PATH — cannot establish the positive control\n' >&2
    exit 77
fi

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-census-blind
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up census-blind || { echo "test-census-blind: could not build a fixture database"; exit 1; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

CENSUS="$HERE/census.sh"

# Locate the real bd binary for env -i calls. SPIRA_BD is the path testdb_up resolved;
# use it as-is, but expand it to an absolute path for environments that rebuild PATH.
_BD_REAL="${SPIRA_BD:-bd}"
case "$_BD_REAL" in
    /*) ;;   # already absolute
    *) _BD_REAL="$(command -v "$_BD_REAL" 2>/dev/null || echo bd)" ;;
esac
_BD_DIR="$(dirname "$_BD_REAL")"

# Seed one bead and one requeue event so census has something to find.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-b1","title":"blind test bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-14T00:00:00Z"}
JSONL
bump_requeue "sp-b1" suite-red

mkdir -p "$TMP/run" "$TMP/home"

# ---------------------------------------------------------------------------
# Census helpers
# ---------------------------------------------------------------------------

# run_census_with_dolt: inherit the current environment so dolt (confirmed on PATH)
# is available to census_events_run_sql.
run_census_with_dolt() {
    SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="$TMP/run" \
        bash "$CENSUS" --with-suppressed 2>/dev/null
}

# NODOLT_BIN: a bin directory with bd (for bdq calls) but without dolt.
# Used in env -i runs that simulate the unreachable-substrate case.
NODOLT_BIN="$TMP/nodolt-bin"
mkdir -p "$NODOLT_BIN"
ln -sf "$_BD_REAL" "$NODOLT_BIN/bd"

# run_census_no_dolt: run census.sh in an environment where dolt is absent.
# SPIRA_PATH controls what conf.sh puts in PATH after it rebuilds it; setting it to
# NODOLT_BIN keeps bd available (for bdq) without making dolt reachable. env -i strips
# any caller-side PATH that might include dolt from an ambient install.
run_census_no_dolt() {
    env -i \
        HOME="$TMP/home" \
        SPIRA_PATH="$NODOLT_BIN" \
        SPIRA_BD="$_BD_REAL" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="$TMP/run" \
        bash "$CENSUS" 2>&1 >/dev/null
}

echo "test-census-blind.sh"

# ==============================================================================
echo
echo "positive control — with dolt on PATH, census exits 0 and reports seeded events:"
# ==============================================================================
# This must be verified before the unreachable test is trusted. If this fails,
# the non-zero exit in the next section could be a fixture fault, not the fix.
pc_out="$(run_census_with_dolt)"
pc_rc=$?
is "census exits 0 with readable substrate" "0" "$pc_rc"
want "seeded class appears in output" "sp-requeue-suite-red" "$pc_out"

# ==============================================================================
echo
echo "unreachable substrate — dolt stripped from PATH, census exits non-zero:"
# ==============================================================================
unreach_stderr="$(run_census_no_dolt 2>&1 || true)"
unreach_rc=0
run_census_no_dolt >/dev/null 2>&1 || unreach_rc=$?

[ "$unreach_rc" -ne 0 ] \
    && ok  "census exits non-zero when substrate is unreachable" \
    || bad "census exits non-zero when substrate is unreachable" "exit code was 0"

# The diagnostic must name the substrate, not the missing binary — census.sh does not
# know which tool is absent, only that both read paths failed.
want "stderr names the substrate" "substrate" "$unreach_stderr"
want "stderr message appears" "unreachable" "$unreach_stderr"

# ==============================================================================
echo
echo "doctor — missing dolt is FAIL on embedded store, WARN on server-mode store:"
# ==============================================================================

# Fake bd that satisfies doctor.sh's database and schema calls without a real server.
FAKE_BIN="$TMP/fake-bin"
mkdir -p "$FAKE_BIN"
cat > "$FAKE_BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *"version"*) printf 'bd v1.2.1\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$FAKE_BIN/bd"

# Fake systemctl that reports all units active so doctor's service checks pass.
cat > "$FAKE_BIN/systemctl" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*) printf 'active\n' ;;
    *"list-units"*|*"list-unit-files"*|*"list-timers"*) true ;;
esac
exit 0
FAKESCRIPT
chmod +x "$FAKE_BIN/systemctl"

# Embedded-store fixture: create a .beads/embeddeddolt directory to mark the store
# as embedded. doctor.sh reads this path directly; no real db initialisation needed.
mkdir -p "$TMP/embedded-db/.beads/embeddeddolt"

# Server-mode fixture: .beads exists but no embeddeddolt.
mkdir -p "$TMP/server-db/.beads"

run_doctor() {     # run_doctor <db-dir>
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$FAKE_BIN" \
        SPIRA_DB="$1" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        bash "$HERE/doctor.sh" 2>/dev/null || true
}

# Embedded store, dolt absent from PATH → FAIL.
embed_out="$(run_doctor "$TMP/embedded-db")"
# Filter to just dolt-related lines for clarity.
embed_dolt_lines="$(printf '%s\n' "$embed_out" | grep -i 'dolt' || true)"
want "embedded store, missing dolt: FAIL line" "FAIL" "$embed_dolt_lines"
nowant "embedded store, missing dolt: not a bare WARN" "warn  dolt" \
       "$(printf '%s\n' "$embed_dolt_lines" | tr '[:upper:]' '[:lower:]' || true)"

# Server-mode store (no embeddeddolt dir), dolt absent → WARN, not FAIL.
server_out="$(run_doctor "$TMP/server-db")"
server_dolt_lines="$(printf '%s\n' "$server_out" | grep -i 'dolt' || true)"
want "server-mode store, missing dolt: warn line" "warn" "$server_dolt_lines"
nowant "server-mode store, missing dolt: no FAIL for dolt" "FAIL" "$server_dolt_lines"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
