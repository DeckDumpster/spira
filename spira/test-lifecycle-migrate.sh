#!/usr/bin/env bash
#
# test-lifecycle-migrate.sh — the migrations release pre-activate runs before every flip
# (sp-vf9iu), against a real Dolt server: `spira-lc admin-migrate --if-enforced` applies a
# pending lifecycle/migrations ADD COLUMN, never re-runs an applied one (0002 is not
# idempotent: a replay fails "duplicate column"), exits non-zero on a failing migration so
# pre-activate refuses the release, and is a no-op where lifecycle is not enforced. And
# admin-apply-ddl selects spira_lifecycle, so a migration file applies as shipped.
# Positive controls: each "absent before" check precedes the "present after" one.
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

"$DOLT_BIN" sql-server --config "$TMP/server.yaml" > "$TMP/server.log" 2>&1 & # batch-job: the suite's own disposable sql-server, killed by cleanup
SERVER_PID=$!

up=0
for _ in $(seq 1 50); do
    if timeout 5 "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        up=1; break
    fi
    sleep 0.2
done
[ "$up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/server.log")"

root_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls "$@"; } # batch-job: fixture SQL against the suite's private server

# spira-lc is the tree under test's own build, found by name on the suite's PATH (sp-gypjk).
command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""


columns() { root_sql --use-db spira_lifecycle sql -q "SHOW COLUMNS FROM bead" -r csv 2>/dev/null; }
reset_db() { root_sql sql -q "DROP DATABASE IF EXISTS spira_lifecycle" >/dev/null 2>&1; }
SHIPPED="$REPO/lifecycle/migrations"
# The live database before round 268: schema.sql as it stood without bead.since.
pre_since_db() {
    reset_db
    grep -v '^    since ' "$REPO/lifecycle/schema.sql" > "$TMP/pre-since.sql"
    spira-lc admin-apply-ddl "$TMP/pre-since.sql" >"$TMP/pre-since.log" 2>&1 || bail "pre-since schema did not apply: $(cat "$TMP/pre-since.log")"
}

echo
echo "a fresh database (schema.sql on an empty server): every shipped migration is moot"
reset_db
spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema.sql applies with no spira_lifecycle yet (admin-apply-ddl falls back to no database)" 0 $?
out="$(spira-lc admin-migrate --if-enforced "$SHIPPED" 2>&1)"; rc=$?
wantrc "admin-migrate on a fresh database succeeds" 0 $rc
want "and skips 0002's column, which schema.sql already made" "0002-since.sql: bead.since present" "$out"

echo
echo "a pending migration is applied before activation, and not re-run after"
pre_since_db
nowant "positive control: the pre-since store lacks since" "since" "$(columns)"
out="$(spira-lc admin-migrate --if-enforced "$SHIPPED" 2>&1)"; rc=$?
wantrc "pending migration applies" 0 $rc
want "reports 0002 applied" "0002-since.sql: added bead.since" "$out"
want "since now exists" "since" "$(columns)"
out="$(spira-lc admin-migrate --if-enforced "$SHIPPED" 2>&1)"; rc=$?
wantrc "a second run succeeds: the non-idempotent 0002 is not replayed" 0 $rc
want "and says why" "0002-since.sql: bead.since present" "$out"

echo
echo "a failing migration exits non-zero and stops the run, so pre-activate refuses the flip"
pre_since_db
MIG="$TMP/migrations"; mkdir -p "$MIG"
printf 'UPDATE no_such_table SET x = 1;\n' > "$MIG/0001-bad.sql"
printf 'ALTER TABLE bead ADD COLUMN since BIGINT NULL;\n' > "$MIG/0002-after.sql"
out="$(spira-lc admin-migrate --if-enforced "$MIG" 2>&1)"; rc=$?
wantrc "admin-migrate refuses" 2 $rc
want "names the failing migration" "0001-bad.sql" "$out"
nowant "the migration after it did not run" "since" "$(columns)"
rm -f "$MIG"/*.sql
printf 'ALTER TABLE bead ADD COLUMN since BIGINT NULL;\n' > "$MIG/0001-good.sql"
printf 'ALTER TABLE bead DROP COLUMN stack_depth;\n' > "$MIG/0002-unguarded.sql"
out="$(spira-lc admin-migrate --if-enforced "$MIG" 2>&1)"; rc=$?
wantrc "an ALTER that cannot be guarded is refused" 2 $rc
want "named" "0002-unguarded.sql" "$out"
nowant "before anything ran (0001 not applied)" "since" "$(columns)"

echo
echo "shipped migrations apply as shipped: admin-apply-ddl selects spira_lifecycle"
pre_since_db
nowant "positive control: pre-since store lacks since" "since" "$(columns)"
spira-lc admin-apply-ddl "$SHIPPED/0002-since.sql" >"$TMP/m2.log" 2>&1
wantrc "0002-since.sql applies via admin-apply-ddl" 0 $?
want "since now exists" "since" "$(columns)"

tl_summary
