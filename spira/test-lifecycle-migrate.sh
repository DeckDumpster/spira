#!/usr/bin/env bash
#
# test-lifecycle-migrate.sh — `spira-lc admin-migrate` applies pending lifecycle/migrations
# in order, records each in the schema_migration ledger, never replays one (0002 is not
# idempotent), and exits non-zero on a failing one so release pre-activate refuses the flip.
# Positive controls: the failing-migration case first proves a good migration does apply.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-lc-hold.sh.
#
# defect: sp-vf9iu
# tier: T1
# covers: lifecycle/schema.sql lifecycle/migrations/* spira-lc/src/** spira/pre-activate.sh
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

. "$HERE/conf.sh"
export PATH="$PATH:$(dirname "$DOLT_BIN")"

# Never let an ambient SPIRA_LC_SOCKET route this suite's calls through a real service.
unset SPIRA_LC_SOCKET

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((SPIRA_LC_TESTDB_PORT + 1000 + (RANDOM % 300)))
SERVER_PID=""
cleanup() { [ -n "$SERVER_PID" ] && kill "$SERVER_PID" >/dev/null 2>&1; rm -rf "$TMP"; }
trap cleanup EXIT INT TERM

mkdir -p "$TMP/data"
cat > "$TMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $PORT
  max_connections: 50
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$TMP/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML

"$DOLT_BIN" sql-server --config "$TMP/server.yaml" > "$TMP/server.log" 2>&1 &
SERVER_PID=$!

up=0
for _ in $(seq 1 50); do
    if "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        up=1; break
    fi
    sleep 0.2
done
[ "$up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/server.log")"

root_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls "$@"; }

# spira-lc is the tree under test's own build, found by name on the suite's PATH (sp-gypjk);
# lifecycle is switched on for this suite with SPIRA_LIFECYCLE_ENFORCE, never by a path.
command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"
export SPIRA_LIFECYCLE_ENFORCE=1

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""


ledger()  { root_sql --use-db spira_lifecycle sql -q "SELECT name FROM schema_migration ORDER BY name" -r csv 2>/dev/null; }
columns() { root_sql --use-db spira_lifecycle sql -q "SHOW COLUMNS FROM bead" -r csv 2>/dev/null; }
reset_db() { root_sql sql -q "DROP DATABASE IF EXISTS spira_lifecycle" >/dev/null 2>&1; }
MIG="$TMP/migrations"; mkdir -p "$MIG"

echo
echo "every shipped migration is named in schema.sql's ledger seed"
for f in "$REPO"/lifecycle/migrations/*.sql; do
    want "schema.sql seeds $(basename "$f")" "'$(basename "$f")'" "$(cat "$REPO/lifecycle/schema.sql")"
done

echo
echo "a fresh database is seeded: nothing is pending, shipped migrations are not replayed"
spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies" 0 $?
out="$(spira-lc admin-migrate "$REPO/lifecycle/migrations" 2>&1)"; rc=$?
wantrc "admin-migrate on a fresh database succeeds" 0 $rc
want "and reports nothing pending" "no pending" "$out"
want "ledger holds 0002" "0002-since.sql" "$(ledger)"

echo
echo "a pending migration is applied once and recorded"
cp "$REPO"/lifecycle/migrations/*.sql "$MIG/"
printf 'USE spira_lifecycle;\nALTER TABLE bead ADD COLUMN probe3 INT NULL;\n' > "$MIG/0003-probe.sql"
nowant "positive control: probe3 is absent before" "probe3" "$(columns)"
out="$(spira-lc admin-migrate "$MIG" 2>&1)"; rc=$?
wantrc "pending migration applies" 0 $rc
want "reports 0003 applied" "applied 0003-probe.sql" "$out"
want "column exists" "probe3" "$(columns)"
want "recorded in the ledger" "0003-probe.sql" "$(ledger)"
out="$(spira-lc admin-migrate "$MIG" 2>&1)"; rc=$?
wantrc "re-running does not replay the non-idempotent migration" 0 $rc
want "nothing pending the second time" "no pending" "$out"

echo
echo "a failing migration exits non-zero, is not recorded, and stops the run"
printf 'USE spira_lifecycle;\nALTER TABLE no_such_table ADD COLUMN x INT NULL;\n' > "$MIG/0004-bad.sql"
printf 'USE spira_lifecycle;\nALTER TABLE bead ADD COLUMN probe5 INT NULL;\n' > "$MIG/0005-after.sql"
out="$(spira-lc admin-migrate "$MIG" 2>&1)"; rc=$?
wantrc "admin-migrate refuses" 2 $rc
want "names the failing migration" "0004-bad.sql FAILED" "$out"
nowant "the failure is not in the ledger" "0004-bad.sql" "$(ledger)"
nowant "the migration after it did not run" "probe5" "$(columns)"
nowant "nor is it recorded" "0005-after.sql" "$(ledger)"

echo
echo "a missing ledger refuses; --baseline creates it without running anything"
reset_db
root_sql sql -q "CREATE DATABASE spira_lifecycle" >/dev/null 2>&1
root_sql --use-db spira_lifecycle sql -q "CREATE TABLE bead (bead_id VARCHAR(8) PRIMARY KEY)" >/dev/null 2>&1
out="$(spira-lc admin-migrate "$MIG" 2>&1)"; rc=$?
wantrc "no ledger: refused" 2 $rc
want "and says how to baseline" "baseline" "$out"
spira-lc admin-migrate --baseline 0001-stack.sql 0002-since.sql >/dev/null 2>&1
wantrc "baseline succeeds" 0 $?
want "baseline recorded 0002" "0002-since.sql" "$(ledger)"
nowant "baseline ran nothing (bead has no since)" "since" "$(columns)"

echo
echo "shipped migrations apply as shipped (they select the database themselves)"
reset_db
grep -v '^    since ' "$REPO/lifecycle/schema.sql" > "$TMP/pre-since.sql"
spira-lc admin-apply-ddl "$TMP/pre-since.sql" >/dev/null 2>&1
nowant "positive control: pre-since schema lacks since" "since" "$(columns)"
spira-lc admin-apply-ddl "$REPO/lifecycle/migrations/0002-since.sql" >"$TMP/m2.log" 2>&1
wantrc "0002-since.sql applies via admin-apply-ddl" 0 $?
want "since now exists" "since" "$(columns)"

tl_summary
