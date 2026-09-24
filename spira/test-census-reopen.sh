#!/usr/bin/env bash
#
# test-census-reopen.sh — census.sh counts distinct beads for sp-reopen, not events.
#
# Two defects in count.py when processing reopened events:
#   (a) Empty new_value: stripping empty fields shifts column positions,
#       so the event count is read as the bead count.
#   (b) Multi-group collapse: summing COUNT(DISTINCT) across groups (one per
#       new_value) double-counts a bead that appears in more than one group.
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail)
# Run against the unfixed tree:
#   FAIL — empty-cause: 1 distinct bead: wanted [1 sp-reopen] in [2 sp-reopen ...]
#   FAIL — multi-value: 1 distinct bead across two new_values: wanted [1 sp-reopen] in [2 sp-reopen ...]
#
# covers: spira/census.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "${2:-}"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-census-reopen
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — seeds reopened events via bd sql directly, which embedded mode refuses
export SPIRA_TESTDB_MODE=server
testdb_up census-reopen || {
    printf 'SKIP test-census-reopen: server testdb not available\n' >&2
    exit 77
}

_PRE_LIB_SPIRA_DB="$SPIRA_DB"
# shellcheck disable=SC1090
. "$HERE/lib.sh"
SPIRA_DB="$_PRE_LIB_SPIRA_DB"; unset _PRE_LIB_SPIRA_DB

echo "test-census-reopen.sh"

census_out() {
    env SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="$TMP/no-run" \
        bash "$HERE/census.sh" --with-suppressed 2>/dev/null
}

seed_bead() {  # seed_bead <id>
    testdb_seed <<JSONL
{"id":"$1","title":"test $1","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-18T00:00:00Z"}
JSONL
}

insert_reopen() {  # insert_reopen <bead_id> [new_value] — NULL when omitted
    local id="$1" nv="${2:-}"
    local uuid nv_sql="NULL"
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))')"
    [ -n "$nv" ] && nv_sql="'$nv'"
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql \
        "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', 'reopened', 'harness', $nv_sql, NOW())" \
        >/dev/null 2>&1
}

# ==============================================================================
echo
echo "POSITIVE CONTROL: seeded reopened event appears in census"
# ==============================================================================
testdb_reset
seed_bead "sp-rpc"
insert_reopen "sp-rpc" '{"status":"open"}'
out_pc="$(census_out)"
want "sp-reopen present when event is seeded" "sp-reopen" "$out_pc"

# ==============================================================================
echo
echo "(a) one bead, two reopened events with empty new_value → 1 distinct bead"
# ==============================================================================
# Pre-fix: count.py strips the empty new_value field, shifting column positions.
# The SQL row "reopened | | 1 | 2" becomes ['reopened','1','2'] after strip,
# so the event count (2) is read as the bead count → census prints "2 sp-reopen".
testdb_reset
seed_bead "sp-ra1"
insert_reopen "sp-ra1"    # NULL new_value
insert_reopen "sp-ra1"    # NULL new_value
out_a="$(census_out)"
want "empty-cause: 1 distinct bead" "1 sp-reopen" "$out_a"

# ==============================================================================
echo
echo "(b) one bead, two new_values → 1 distinct bead"
# ==============================================================================
# Pre-fix: SQL groups by (event_type, new_value) → two groups, each reporting
# 1 distinct bead. count.py sums across groups → 2, double-counting the same bead.
testdb_reset
seed_bead "sp-rb1"
insert_reopen "sp-rb1"                        # NULL new_value
insert_reopen "sp-rb1" '{"status":"open"}'    # non-empty new_value
out_b="$(census_out)"
want "multi-value: 1 distinct bead across two new_values" "1 sp-reopen" "$out_b"
want "multi-value: 2 events detected" "sp-reopen-unrecorded (2 detections" "$out_b"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
