#!/usr/bin/env bash
#
# test-events-nodolt.sh — events file fallback when neither bd sql nor dolt CLI is available.
#
#   ./test-events-nodolt.sh
#
# WHAT THIS SUITE GUARDS
# ----------------------
# On an embedded-Dolt install where the dolt CLI is absent and bd is the CGO_ENABLED=0
# binary (which refuses bd sql in embedded mode), all three events functions silently
# discarded every write before db-wx4.  Reads answered 0; census reported nothing.
# The recurrence counter was permanently frozen at 1, so sin-escalation was unreachable.
#
# This suite verifies the file fallback introduced by db-wx4: when both SQL paths fail,
# _bump_write_event appends to $SPIRA_DB/events.log, and _counter_events_query /
# census_events_run_sql read it back.  It runs without a real bd database by:
#
#   1. Creating a fake embedded store (just the .beads/embeddeddolt directory).
#   2. Using a bd wrapper that refuses "sql" to simulate the CGO_ENABLED=0 binary.
#   3. Running in a PATH that has no dolt.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# --------------------------------------------------------
# The suite was run against the unfixed tree; recurs_of returned 0 after three
# bump_recur calls, and census_events_run_sql produced no output.
#
# covers: spira/lib.sh spira/census.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---- Build the "embedded Dolt, no dolt CLI, no bd sql" environment ----
#
# Fake embedded store: only the .beads/embeddeddolt directory needs to exist.
# The three lib.sh functions test for it to know they are talking to embedded Dolt.
FAKE_DB="$TMP/db"
mkdir -p "$FAKE_DB/.beads/embeddeddolt"

# bd wrapper: refuses "sql" (simulates the CGO_ENABLED=0 binary), passes "list" with
# an empty JSON array so census.sh's _suppressed_classes pipeline exits 0.
BD_REAL="$(command -v bd 2>/dev/null)" || BD_REAL="bd"
cat > "$TMP/bd-nosql" <<WRAPPER
#!/usr/bin/env bash
for arg in "\$@"; do
    if [ "\$arg" = "sql" ]; then
        printf "Error: 'bd sql' is not yet supported in embedded mode\n" >&2
        exit 1
    fi
    if [ "\$arg" = "list" ]; then
        printf '[]\n'
        exit 0
    fi
done
exec "$BD_REAL" "\$@"
WRAPPER
chmod +x "$TMP/bd-nosql"

# PATH without dolt: keep every directory the test process uses, minus any that
# contain a dolt binary, plus our wrapper directory at the front.
_SAFE_PATH="$TMP"
IFS=: read -ra _pathdirs <<< "$PATH"
for _d in "${_pathdirs[@]}"; do
    [ -n "$_d" ] || continue
    [ -x "$_d/dolt" ] && continue
    _SAFE_PATH="${_SAFE_PATH}:${_d}"
done

export SPIRA_BD="$TMP/bd-nosql"
export SPIRA_DB="$FAKE_DB"

# shellcheck disable=SC1090
. "$HERE/lib.sh"
# conf.sh (sourced inside lib.sh) resets PATH to include /usr/local/bin where dolt lives.
# Re-apply the no-dolt restriction AFTER lib.sh so the file-fallback path is exercised.
export PATH="$_SAFE_PATH"

echo "test-events-nodolt.sh"

# ======================================================================================
echo
echo "positive control — confirm both SQL paths are inoperative"
# ======================================================================================
_bd_sql_rc=0
"$SPIRA_BD" -C "$SPIRA_DB" sql "SELECT 1" >/dev/null 2>&1 || _bd_sql_rc=$?
is "bd sql is refused (simulating CGO_ENABLED=0)" "1" "$_bd_sql_rc"

_dolt_found=0
command -v dolt >/dev/null 2>&1 && _dolt_found=1 || true
is "dolt is absent from PATH" "0" "$_dolt_found"

is "embeddeddolt directory exists (fake embedded store)" "1" \
    "$([ -d "$FAKE_DB/.beads/embeddeddolt" ] && echo 1 || echo 0)"

# ======================================================================================
echo
echo "bump_recur round-trip — db-wx4 acceptance criteria"
# ======================================================================================
is "recurs_of returns 0 before any bumps" "0" "$(recurs_of sp-nd-1)"
is "events.log does not exist before first bump" "0" \
    "$([ -f "$FAKE_DB/events.log" ] && echo 1 || echo 0)"

bump_recur "sp-nd-1" suite-red
bump_recur "sp-nd-1" suite-red
bump_recur "sp-nd-1" suite-red

is "events.log was created by bump_recur" "1" \
    "$([ -f "$FAKE_DB/events.log" ] && echo 1 || echo 0)"
is "recurs_of reads back 3 (db-wx4 acceptance criterion)" "3" "$(recurs_of sp-nd-1)"

# ======================================================================================
echo
echo "bump_recur with a second bead — no cross-contamination"
# ======================================================================================
bump_recur "sp-nd-2" merge-conflict
bump_recur "sp-nd-2" merge-conflict

is "recurs_of sp-nd-2 reads 2" "2" "$(recurs_of sp-nd-2)"
is "recurs_of sp-nd-1 still 3 after sp-nd-2 bumps" "3" "$(recurs_of sp-nd-1)"

# ======================================================================================
echo
echo "bump_reclaim and bump_requeue round-trip"
# ======================================================================================
bump_reclaim "sp-nd-3" timeout
bump_reclaim "sp-nd-3" timeout
is "reclaims_of reads 2" "2" "$(reclaims_of sp-nd-3)"

bump_requeue "sp-nd-4" gate-red
is "requeues_of reads 1" "1" "$(requeues_of sp-nd-4)"

bump_requeue "sp-nd-5" thrash
is "requeues_of for thrash cause reads 1 (db-ipo)" "1" "$(requeues_of sp-nd-5)"

bump_lapsed "sp-nd-6" lease-expired

# ======================================================================================
echo
echo "census_events_run_sql — file fallback output is parseable by census.sh count.py"
# ======================================================================================
_craw="$(census_events_run_sql)"
want "output contains event_type 'recurred'" "recurred"    "$_craw"
want "output contains cause 'suite-red'"     "suite-red"   "$_craw"
want "output contains count 3"               "| 3 |"       "$_craw"
want "output contains 'reclaimed'"           "reclaimed"   "$_craw"
want "output contains 'lapsed' (db-ipo filter fix)" "lapsed" "$_craw"

# ======================================================================================
echo
echo "census_events_run_sql — since-watermark filter"
# ======================================================================================
# A since value in the far future excludes all events written so far.
_future=9999999999
_out_future="$(census_events_run_sql "$_future")"
is "since=far-future returns empty (all events are older)" "" "$_out_future"

# A since value of 0 returns all events.
_out_all="$(census_events_run_sql 0)"
want "since=0 returns all events" "suite-red" "$_out_all"

# ======================================================================================
echo
echo "census.sh end-to-end — db-wx4 acceptance criterion"
# ======================================================================================
mkdir -p "$TMP/run"
_cens="$(SPIRA_DB="$FAKE_DB" SPIRA_BD="$TMP/bd-nosql" SPIRA_RUN="$TMP/run" \
    bash "$HERE/census.sh" --with-suppressed 2>/dev/null)"
want "census reports 3 sp-recur-suite-red (db-wx4 acceptance criterion)" \
    "3 sp-recur-suite-red" "$_cens"
want "census reports 2 sp-recur-merge-conflict" "2 sp-recur-merge-conflict" "$_cens"
want "census reports 2 sp-reclaim-timeout"      "2 sp-reclaim-timeout"      "$_cens"
want "census reports 1 sp-requeue-thrash (db-ipo acceptance criterion)" \
    "1 sp-requeue-thrash" "$_cens"
want "census reports 1 sp-lapsed-lease-expired (db-ipo lapsed filter fix)" \
    "1 sp-lapsed-lease-expired" "$_cens"

printf '\ntest-events-nodolt.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
