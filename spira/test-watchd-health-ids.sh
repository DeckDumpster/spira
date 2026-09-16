#!/usr/bin/env bash
#
# test-watchd-health-ids.sh — health-ids reads the prefix from the database, not the goal bead.
#
# THE BUG. watchd.sh health-ids proved a watcher could see *our* beads by grepping its
# state file for an id carrying SPIRA_ID_PREFIX. That variable was derived from the goal
# bead (SPIRA_GOAL%%-*), not from the database. On any installation where the two differ —
# because the database was initialised in a directory whose name is not the bead prefix —
# the assertion was permanently DEGRADED for an unrelated reason, masking real failures as
# indistinguishable noise (law-alerts-must-be-actionable).
#
# WHAT THIS HOLDS:
#   1. POSITIVE CONTROL: a file with no database-prefix ids FAILS first. This proves the
#      check is not trivially satisfied, so the passing case below is evidence.
#   2. GOAL-DRIFT CASE: SPIRA_GOAL carries a different prefix than the database. health-ids
#      should read the prefix from the database and PASS on a file that contains the
#      database's own ids — the exact case that was permanently DEGRADED before the fix.
#   3. FILE MISSING: DEGRADED (exit 1), named so the fault is actionable.
#   4. FALLBACK PASS: no database available but SPIRA_ID_PREFIX is set correctly — works.
#   5. FALLBACK FAIL: no database, SPIRA_ID_PREFIX wrong — correctly DEGRADED.
#
# Uses a real throwaway database so the prefix is authoritative (law-prefer-the-real-dependency).
#
# defect: sp-c57o
# covers: spira/watchd.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
WATCHD="$HERE/watchd.sh"
. "$HERE/testdb.sh"

pass=0; fail=0
ok()  { printf '  ok    %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n        got: %s\n' "$1" "$2"; fail=$((fail+1)); }
is()  { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "want [$2] got [$3]"; fi; }
has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "$2" ;; esac; }

TMP="$(mktemp -d)"
testdb_require health-ids
testdb_up health-ids || { echo "SKIP: no bd engine" >&2; exit 0; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

# Get the actual prefix the database issues — the authority for what ids look like.
db_prefix="$("$SPIRA_BD" -C "$SPIRA_DB" config get issue_prefix 2>/dev/null)" || db_prefix=""
[ -n "$db_prefix" ] || { echo "SKIP: bd config get issue_prefix returned nothing" >&2; exit 0; }

# A goal bead whose prefix is deliberately different from the database's — the exact
# mismatch that caused the false DEGRADED. SPIRA_ID_PREFIX is not set in the env, so
# conf.sh will derive it from SPIRA_GOAL. Without the fix, cmd_health_ids would use that
# derived value and look for the wrong prefix.
GOAL_PREFIX="notthedb"

# A state file with ids from the database's own prefix: what a healthy watcher produces.
STATE_GOOD="$TMP/state-good.json"
printf '{"seen":["%s-abc1","%s-xyz2"]}\n' "$db_prefix" "$db_prefix" > "$STATE_GOOD"

# A state file with no database-prefix ids: simulates a watcher reading the wrong database.
STATE_NONE="$TMP/state-none.json"
printf '{"seen":["other-abc1","other-xyz2"]}\n' > "$STATE_NONE"

run_ids() {   # run_ids <file> [extra env vars...]
    local f="$1"; shift
    local out rc
    out="$(SPIRA_GOAL="${GOAL_PREFIX}-goal" SPIRA_BD="$SPIRA_BD" SPIRA_DB="$SPIRA_DB" \
        SPIRA_CONF=/nonexistent "$@" bash "$WATCHD" health-ids "$f" 2>&1)"
    rc=$?
    printf '%s\n%s' "$out" "$rc"
}

echo "positive control: file with no database-prefix ids must fail"
result="$(run_ids "$STATE_NONE")"
rc="${result##*$'\n'}"; msg="${result%$'\n'*}"
is "no-match file exits 1" 1 "$rc"
has "no-match message names the db prefix" "$msg" "${db_prefix}-"

echo "goal-drift: SPIRA_GOAL prefix differs from database prefix"
result="$(run_ids "$STATE_GOOD")"
rc="${result##*$'\n'}"
is "correct-prefix file exits 0" 0 "$rc"

echo "missing state file"
out="$(SPIRA_BD="$SPIRA_BD" SPIRA_DB="$SPIRA_DB" SPIRA_CONF=/nonexistent \
    bash "$WATCHD" health-ids "$TMP/no-such-file.json" 2>&1)"; rc=$?
is "missing file exits 1" 1 "$rc"
has "missing file message says does not exist" "$out" "does not exist"

echo "no database: fallback to SPIRA_ID_PREFIX when it is correct"
out="$(SPIRA_ID_PREFIX="$db_prefix" SPIRA_BD="" SPIRA_DB="" SPIRA_CONF=/nonexistent \
    bash "$WATCHD" health-ids "$STATE_GOOD" 2>&1)"; rc=$?
is "fallback with correct prefix exits 0" 0 "$rc"

echo "no database: fallback to SPIRA_ID_PREFIX when it is wrong"
out="$(SPIRA_GOAL="${GOAL_PREFIX}-goal" SPIRA_BD="" SPIRA_DB="" SPIRA_CONF=/nonexistent \
    bash "$WATCHD" health-ids "$STATE_GOOD" 2>&1)"; rc=$?
is "fallback with wrong prefix exits 1" 1 "$rc"
has "wrong prefix message identifies the prefix" "$out" "${GOAL_PREFIX}-"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
