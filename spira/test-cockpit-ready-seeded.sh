#!/usr/bin/env bash
#
# test-cockpit-ready-seeded.sh — SP_READY is non-zero when beads are ready.
#
# POSITIVE CONTROL for the mock-bd tests in test-cockpit-ready.sh. Those tests verify
# SP_READY=0 on an empty db and SP_READY=? on a refusal, but neither proves that the
# probe reads a real non-zero count against a real database. This suite seeds 2 ready
# beads on a real bd-embedded instance and asserts SP_READY=2 — a check that cannot be
# "aimed at the wrong thing" (law-absence-needs-a-positive-control).
#
# defect: sp-axn
# covers: spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require test-cockpit-ready-seeded
TMP="$(mktemp -d)"
RUN="$TMP/run"; mkdir -p "$RUN"
testdb_up ready_seeded || { echo "test-cockpit-ready-seeded: could not build fixture database"; exit 1; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# Run cockpit.sh core against the real fixture database with builder persona active.
# SPIRA_FAYTHS=builder resolves to chamber/builder.fayth, so the partition map is real
# and core_detail_keys emits SP_READY from the actual query results.
run_core() {
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-goal SPIRA_FAYTHS=builder \
        SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_BD="$SPIRA_BD" \
        bash "$HERE/cockpit.sh" core 2>/dev/null
}

echo "test-cockpit-ready-seeded.sh"

# ======================================================================================
# NEGATIVE CONTROL: empty database — SP_READY must be 0, not ?.
# A probe that reads ? on an empty database is broken; 0 is the correct answer here.
echo
echo "empty database — SP_READY must be 0:"
testdb_reset
out="$(run_core)"
want   "SP_READY is present on empty db"       "SP_READY="  "$out"
nowant "SP_READY is not ? on empty db"         "SP_READY=?" "$out"
is     "SP_READY is 0 with no beads"           "0" \
       "$(printf '%s\n' "$out" | grep -m1 '^SP_READY=' | sed 's/^SP_READY=//')"

# ======================================================================================
# POSITIVE CONTROL: 2 ready beads seeded — SP_READY must equal 2.
# Without this, a probe that always returns 0 passes the negative control above.
echo
echo "2 ready beads seeded — SP_READY must be 2:"
testdb_reset
testdb_seed <<JSONL
{"id":"sp-r1","title":"ready bead one","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-08T00:00:00Z"}
{"id":"sp-r2","title":"ready bead two","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-08T00:00:00Z"}
JSONL
out="$(run_core)"
sp_ready="$(printf '%s\n' "$out" | grep -m1 '^SP_READY=' | sed 's/^SP_READY=//')"
want   "SP_READY is present with seeded beads"   "SP_READY="  "$out"
nowant "SP_READY is not ? with seeded beads"     "SP_READY=?" "$out"
is     "SP_READY is 2 (non-zero)"                "2"          "$sp_ready"
is     "positive control is real (non-zero)"     "2"          "$sp_ready"

echo
printf 'test-cockpit-ready-seeded: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
