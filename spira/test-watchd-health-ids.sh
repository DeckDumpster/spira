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
#   6. UNUSABLE PREFIX (UC-operator-channel-34, gap G-09): a prefix that resolves to empty
#      or carries a non-alphanumeric character exits 2, distinct from the DEGRADED exit 1 —
#      the check could not run at all, which is a different fact from a bad state file.
#
# The database dependency is a stub script honouring SPIRA_BD (the seam cmd_health_ids
# already reads), not a real bd engine: the prefix lookup is one command with one output,
# and a real database bought nothing this suite used except several seconds of startup
# (test-plan-2026-09-23 §7's profiling note on this file's prior T2 cost).
#
# defect: sp-c57o
# tier: T1
# covers: spira/watchd.sh UC-operator-channel-34
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
WATCHD="$HERE/watchd.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# A stub "bd" implementing only what cmd_health_ids calls: `-C <db> config get issue_prefix`.
# It prints $STUB_ISSUE_PREFIX regardless of its arguments — the prefix lookup is the only
# thing under test, not bd's argument parsing.
STUB_BD="$TMP/bd-stub.sh"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "${STUB_ISSUE_PREFIX-}"
STUB
chmod +x "$STUB_BD"

DB_PREFIX="sptest"
GOAL_PREFIX="notthedb"

# A state file with ids under the stub database's own prefix: what a healthy watcher produces.
STATE_GOOD="$TMP/state-good.json"
printf '{"seen":["%s-abc1","%s-xyz2"]}\n' "$DB_PREFIX" "$DB_PREFIX" > "$STATE_GOOD"

# A state file with no database-prefix ids: simulates a watcher reading the wrong database.
STATE_NONE="$TMP/state-none.json"
printf '{"seen":["other-abc1","other-xyz2"]}\n' > "$STATE_NONE"

run_ids() {   # run_ids <file> [extra env vars...]
    local f="$1"; shift
    local out rc
    out="$(SPIRA_GOAL="${GOAL_PREFIX}-goal" SPIRA_BD="$STUB_BD" SPIRA_DB="$TMP/db" \
        STUB_ISSUE_PREFIX="$DB_PREFIX" SPIRA_CONF=/nonexistent "$@" bash "$WATCHD" health-ids "$f" 2>&1)"
    rc=$?
    printf '%s\n%s' "$out" "$rc"
}

echo "positive control: file with no database-prefix ids must fail"
result="$(run_ids "$STATE_NONE")"
rc="${result##*$'\n'}"; msg="${result%$'\n'*}"
is "no-match file exits 1" 1 "$rc"
want "no-match message names the db prefix" "${DB_PREFIX}-" "$msg"

echo "goal-drift: SPIRA_GOAL prefix differs from database prefix"
result="$(run_ids "$STATE_GOOD")"
rc="${result##*$'\n'}"
is "correct-prefix file exits 0" 0 "$rc"

echo "missing state file"
out="$(SPIRA_BD="$STUB_BD" SPIRA_DB="$TMP/db" STUB_ISSUE_PREFIX="$DB_PREFIX" SPIRA_CONF=/nonexistent \
    bash "$WATCHD" health-ids "$TMP/no-such-file.json" 2>&1)"; rc=$?
is "missing file exits 1" 1 "$rc"
want "missing file message says does not exist" "does not exist" "$out"

echo "no database: fallback to SPIRA_ID_PREFIX when it is correct"
out="$(SPIRA_ID_PREFIX="$DB_PREFIX" SPIRA_BD="" SPIRA_DB="" SPIRA_CONF=/nonexistent \
    bash "$WATCHD" health-ids "$STATE_GOOD" 2>&1)"; rc=$?
is "fallback with correct prefix exits 0" 0 "$rc"

echo "no database: fallback to SPIRA_ID_PREFIX when it is wrong"
out="$(SPIRA_GOAL="${GOAL_PREFIX}-goal" SPIRA_BD="" SPIRA_DB="" SPIRA_CONF=/nonexistent \
    bash "$WATCHD" health-ids "$STATE_GOOD" 2>&1)"; rc=$?
is "fallback with wrong prefix exits 1" 1 "$rc"
want "wrong prefix message identifies the prefix" "${GOAL_PREFIX}-" "$out"

echo "gap G-09 / UC-operator-channel-34: unusable prefix exits 2, not 1 and not 0"
# SPIRA_GOAL must derive an empty prefix too, not merely be unset or empty:
# conf.sh defaults SPIRA_GOAL itself to sp-spira with the same := form, so an
# empty SPIRA_GOAL would silently re-derive the ambient "sp" prefix and hide
# this case. A value with no text before its first hyphen (${GOAL%%-*}, the
# same derivation cmd_health_ids's caller relies on) is non-empty and so
# skips that default, while still deriving an empty prefix.
out="$(SPIRA_BD="" SPIRA_DB="" SPIRA_ID_PREFIX="" SPIRA_GOAL="-nogoal" SPIRA_CONF=/nonexistent \
    bash "$WATCHD" health-ids "$STATE_GOOD" 2>&1)"; rc=$?
is "no prefix anywhere exits 2" 2 "$rc"
want "no-prefix message says how to fix it" "SPIRA_ID_PREFIX" "$out"

out="$(SPIRA_BD="" SPIRA_DB="" SPIRA_ID_PREFIX="bad prefix" SPIRA_CONF=/nonexistent \
    bash "$WATCHD" health-ids "$STATE_GOOD" 2>&1)"; rc=$?
is "non-alphanumeric prefix exits 2" 2 "$rc"

out="$(SPIRA_BD="" SPIRA_DB="" SPIRA_ID_PREFIX="sp-test" SPIRA_CONF=/nonexistent \
    bash "$WATCHD" health-ids "$STATE_GOOD" 2>&1)"; rc=$?
is "hyphenated prefix exits 2" 2 "$rc"

tl_summary
