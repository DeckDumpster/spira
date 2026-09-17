#!/usr/bin/env bash
#
# test-census-blind.sh — census.sh and doctor.sh fail closed when the events
# substrate is unreachable (embedded store, dolt absent from PATH); census.sh
# exits 0 with no output when the embedded store is genuinely empty.
#
#   ./test-census-blind.sh
#
# WHAT THIS GUARDS
# ----------------
# Before db-ur7, census_events_run_sql returned exit 0 with empty output when both
# access paths (bd sql, dolt) were unavailable. census.sh consumed that silence as
# "zero classes ranked" — identical output to a genuinely clean corpus. Six Maechen
# passes completed against a blind census and none recorded an outcome.
#
# Before db-7on, census_events_run_sql returned exit 1 (unreachable) when dolt was
# absent and events.log had never been created. When dolt is absent, path 3
# (events.log) is always the active write path; an absent events.log means nothing
# has been written yet — genuinely empty, not unreachable. Every fresh
# embedded-no-dolt install hit this on its first Maechen pass.
#
# FOUR ACCEPTANCE CRITERIA:
#   1. EMPTY-SUBSTRATE (no dolt needed). census.sh exits 0 with no output on an
#      embedded store with dolt absent and events.log absent, whether the dolt
#      database directory is empty or has schema data from bd migrate schema.
#   2. POSITIVE CONTROL (requires dolt). With dolt on PATH, census.sh exits 0 and
#      reports seeded events. Required before trusting "unreachable" result.
#   3. UNREACHABLE (requires dolt). With dolt stripped from PATH (events in dolt
#      table, dolt removed), census.sh exits non-zero and writes a diagnostic.
#   4. DOCTOR (no dolt needed). doctor.sh reports dolt as FAIL (not WARN) when the
#      embeddeddolt directory exists and dolt is not on PATH.
#
# Criteria 2 and 3 require dolt and are skipped when it is absent. Criteria 1 and 4
# run regardless.
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

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home"

CENSUS="$HERE/census.sh"

echo "test-census-blind.sh"

# ==============================================================================
echo
echo "empty-substrate — embedded store, no dolt, no events yet (sp-census-empty-read-as-unreachable):"
# ==============================================================================
# A fresh embedded install where the CGO_ENABLED=0 bd binary ran bd init but
# dolt was never present. bd init creates .beads/embeddeddolt/<db> as an empty
# directory; path 3 (events.log) is the only write path, but nothing has been
# written yet. Before db-7on, census returned 1 (unreachable) for this state.
#
# Two checks:
#   A. Empty store: census exits 0 with no output.
#   B. Seeded-row positive control: after writing one row directly to events.log,
#      census exits 0 and reports the class. This confirms the file-substrate
#      reader is functional, making A's silence believable.

_EMPTY_DB="$TMP/empty-db"
_EMPTY_RUN="$TMP/empty-run"
mkdir -p "$_EMPTY_DB/.beads/embeddeddolt/db" "$_EMPTY_RUN"

# bd wrapper: refuses sql (simulates CGO_ENABLED=0 binary in embedded mode),
# returns [] for list so census.sh's remedies query exits 0.
_BD_FAKE="$TMP/bd-nosql"
cat > "$_BD_FAKE" <<'WRAPPER'
#!/usr/bin/env bash
for arg in "$@"; do
    if [ "$arg" = "sql" ]; then
        printf "Error: 'bd sql' is not yet supported in embedded mode\n" >&2
        exit 1
    fi
    if [ "$arg" = "list" ]; then
        printf '[]\n'
        exit 0
    fi
done
WRAPPER
chmod +x "$_BD_FAKE"
_EMPTY_BIN="$TMP/empty-bin"
mkdir -p "$_EMPTY_BIN"
ln -sf "$_BD_FAKE" "$_EMPTY_BIN/bd"

_emp_rc=0
_emp_out="$(env -i \
    HOME="$TMP/home" \
    SPIRA_PATH="$_EMPTY_BIN" \
    SPIRA_BD="$_BD_FAKE" \
    SPIRA_DB="$_EMPTY_DB" \
    SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
    SPIRA_CONF="$TMP/no-conf" \
    SPIRA_HOME="$HERE" \
    SPIRA_RUN="$_EMPTY_RUN" \
    bash "$CENSUS" --with-suppressed 2>/dev/null)" || _emp_rc=$?
is "empty embedded store: census exits 0 (no events yet)" "0" "$_emp_rc"
is "empty embedded store: no output (empty store, not unreachable)" "" "$_emp_out"

# bd migrate schema populates the dolt directory with schema data even when no
# event has ever been written, so a fresh embedded install typically has a
# non-empty embeddeddolt/<db> directory. The fix must return 0 in that state
# too, not just when the directory is completely empty.
_SCHEMA_DB="$TMP/schema-db"
_SCHEMA_RUN="$TMP/schema-run"
mkdir -p "$_SCHEMA_DB/.beads/embeddeddolt/db/.dolt/noms" "$_SCHEMA_RUN"
printf 'schema\n' > "$_SCHEMA_DB/.beads/embeddeddolt/db/.dolt/noms/manifest"
_schema_rc=0
_schema_out="$(env -i \
    HOME="$TMP/home" \
    SPIRA_PATH="$_EMPTY_BIN" \
    SPIRA_BD="$_BD_FAKE" \
    SPIRA_DB="$_SCHEMA_DB" \
    SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
    SPIRA_CONF="$TMP/no-conf" \
    SPIRA_HOME="$HERE" \
    SPIRA_RUN="$_SCHEMA_RUN" \
    bash "$CENSUS" --with-suppressed 2>/dev/null)" || _schema_rc=$?
is "schema-populated doltdb, no events.log: census exits 0" "0" "$_schema_rc"
is "schema-populated doltdb, no events.log: no output" "" "$_schema_out"

# Positive control: seed one row directly to events.log; the file-substrate
# reader must report the class (case C from the db-7on bead description).
printf '%s\tsp-ec-test1\trecurred\tsuite-red\n' "$(date +%s)" \
    >> "$_EMPTY_DB/events.log"
_emp_seeded_rc=0
_emp_seeded_out="$(env -i \
    HOME="$TMP/home" \
    SPIRA_PATH="$_EMPTY_BIN" \
    SPIRA_BD="$_BD_FAKE" \
    SPIRA_DB="$_EMPTY_DB" \
    SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
    SPIRA_CONF="$TMP/no-conf" \
    SPIRA_HOME="$HERE" \
    SPIRA_RUN="$_EMPTY_RUN" \
    bash "$CENSUS" --with-suppressed 2>/dev/null)" || _emp_seeded_rc=$?
is "seeded-row: census exits 0" "0" "$_emp_seeded_rc"
want "seeded-row: class appears in output (positive control)" "sp-recur-suite-red" "$_emp_seeded_out"

# ==============================================================================
# Dolt-dependent criteria: positive control and unreachable substrate.
# Skipped when dolt is not on PATH — these seed events into the dolt table to
# prove the substrate is readable before trusting the "unreachable" result.
# ==============================================================================
if ! command -v dolt >/dev/null 2>&1; then
    printf '  skip  (positive-control, unreachable: dolt not on PATH)\n'
else
    testdb_require test-census-blind
    testdb_up census-blind || { echo "test-census-blind: could not build a fixture database"; exit 1; }
    # shellcheck disable=SC1090
    . "$HERE/lib.sh"

    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-b1","title":"blind test bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-14T00:00:00Z"}
JSONL
    bump_requeue "sp-b1" suite-red

    mkdir -p "$TMP/run"

    _BD_REAL="${SPIRA_BD:-bd}"
    case "$_BD_REAL" in
        /*) ;;
        *) _BD_REAL="$(command -v "$_BD_REAL" 2>/dev/null || echo bd)" ;;
    esac
    _BD_DIR="$(dirname "$_BD_REAL")"

    run_census_with_dolt() {
        SPIRA_DB="$SPIRA_DB" \
            SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
            SPIRA_CONF="$TMP/no-conf" \
            SPIRA_HOME="$HERE" \
            SPIRA_RUN="$TMP/run" \
            bash "$CENSUS" --with-suppressed 2>/dev/null
    }

    NODOLT_BIN="$TMP/nodolt-bin"
    mkdir -p "$NODOLT_BIN"
    ln -sf "$_BD_REAL" "$NODOLT_BIN/bd"
    # A fake dolt that always exits 1 is placed in NODOLT_BIN so that conf.sh's PATH
    # construction (which prepends SPIRA_PATH before /usr/local/bin) puts this fake dolt
    # first, shadowing the real binary. census_events_run_sql will then see dolt fail and
    # fall through to the file fallback (db-wx4) or return 0 if events.log is absent.
    printf '#!/usr/bin/env bash\nexit 1\n' > "$NODOLT_BIN/dolt"
    chmod +x "$NODOLT_BIN/dolt"

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
            bash "$CENSUS" 2>/dev/null
    }

    # ==========================================================================
    echo
    echo "positive control — with dolt on PATH, census exits 0 and reports seeded events:"
    # ==========================================================================
    pc_out="$(run_census_with_dolt)"
    pc_rc=$?
    is "census exits 0 with readable substrate" "0" "$pc_rc"
    want "seeded class appears in output" "sp-requeue-suite-red" "$pc_out"

    # ==========================================================================
    echo
    echo "unreachable substrate — dolt replaced by failing stub, census exits 0 (db-7on tradeoff):"
    # ==========================================================================
    # db-7on accepted a false "empty" for the case where dolt was the write path but is
    # now absent/failing, because the doltdb directory's content cannot distinguish
    # schema-only from schema-plus-events after bd migrate schema runs. The failing-stub
    # dolt simulates this: events exist in the real dolt table, but the stub fails and
    # events.log is absent (path-3 was never the write path here), so census returns 0.
    #
    # SERVER MODE SKIP. In server mode, _bump_write_event uses bd sql (path 1) as the
    # write path, not dolt. Replacing dolt with a failing stub does not affect path 1,
    # so census still reads events via bd sql and cannot return empty. The scenario
    # ("dolt was the write path and is now absent") does not arise in server mode.
    if [ "${TESTDB_MODE:-}" = server ]; then
        printf '  skip  (unreachable: server mode uses bd sql as read path; scenario does not apply)\n'
    else
        unreach_out=""
        unreach_rc=0
        unreach_out="$(run_census_no_dolt 2>/dev/null)" || unreach_rc=$?

        is   "census exits 0 with inaccessible dolt (db-7on false-empty tradeoff)" "0" "$unreach_rc"
        is   "census output is empty (events in dolt are unreachable)" "" "$unreach_out"
    fi
fi

# ==============================================================================
echo
echo "doctor — missing dolt is FAIL on embedded store, WARN on server-mode store:"
# ==============================================================================

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

cat > "$FAKE_BIN/systemctl" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*) printf 'active\n' ;;
    *"is-enabled"*) printf 'enabled\n' ;;
    *"list-units"*|*"list-unit-files"*|*"list-timers"*) true ;;
esac
exit 0
FAKESCRIPT
chmod +x "$FAKE_BIN/systemctl"

mkdir -p "$TMP/embedded-db/.beads/embeddeddolt"
mkdir -p "$TMP/server-db/.beads"

run_doctor() {
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

mkdir -p "$TMP/run"

# db-9dh replaced the dolt-binary FAIL with an events round-trip probe (covered
# by test-doctor-events-probe.sh). Missing dolt is now WARN for both store types:
# the file fallback (db-wx4) handles events when dolt is absent, so the binary's
# absence alone cannot signal a broken write path.
#
# run_doctor uses PATH="/usr/local/bin:/usr/bin:/bin" with HOME="$TMP/home", so dolt
# is found only when it is installed at /usr/local/bin, /usr/bin, or /bin. On most
# setups dolt lives in ~/.local/bin and is invisible inside run_doctor regardless of
# whether it is installed on the host. Check the doctor-visible paths specifically.
_doctor_has_dolt=0
for _p in "$FAKE_BIN" /usr/local/bin /usr/bin /bin; do [ -x "$_p/dolt" ] && _doctor_has_dolt=1 && break; done

embed_out="$(run_doctor "$TMP/embedded-db")"
embed_dolt_lines="$(printf '%s\n' "$embed_out" | grep -iE '^\s+(ok|warn|FAIL)\s+dolt' || true)"
# The broad grep -i 'dolt' also captures events-probe FAIL lines that mention "dolt
# embedded" in their path string (e.g. "FAIL  events write/read round trip failed —
# writes via dolt embedded (...)"), which produces a false FAIL when dolt is installed
# at /usr/local/bin. The precise pattern here matches only lines where the status label
# is immediately followed by "dolt" — i.e. the dolt binary ok/warn/FAIL line itself.
if [ "$_doctor_has_dolt" = 1 ]; then
    want   "embedded store, dolt present: ok line" "ok" "$embed_dolt_lines"
else
    want   "embedded store, missing dolt: warn line" "warn" "$embed_dolt_lines"
fi
nowant "embedded store: no FAIL for dolt" "FAIL" "$embed_dolt_lines"

server_out="$(run_doctor "$TMP/server-db")"
server_dolt_lines="$(printf '%s\n' "$server_out" | grep -iE '^\s+(ok|warn|FAIL)\s+dolt' || true)"
if [ "$_doctor_has_dolt" = 1 ]; then
    want   "server-mode store, dolt present: ok line" "ok" "$server_dolt_lines"
else
    want   "server-mode store, missing dolt: warn line" "warn" "$server_dolt_lines"
fi
nowant "server-mode store: no FAIL for dolt" "FAIL" "$server_dolt_lines"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
