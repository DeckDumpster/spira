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
# sp-p1z81: the fixture is production's shape — root has a password, the lifecycle service
# user spira_lc comes from grants.sql — and "already applied" is read as spira_lc, so a store
# that carries every migration passes with no admin credential; only a pending migration
# needs SPIRA_LC_ADMIN_USER/SPIRA_LC_ADMIN_PASSWORD, and without them (or with them wrong)
# the run refuses naming exactly those variables and never prints a password.
# A pending migration made only of guarded DML on tables the service user writes (0003's
# UPDATE bead ... WHERE) needs no admin: the service user applies it itself.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-lc-hold.sh.
#
# defect: sp-vf9iu sp-p1z81
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

ROOTPW="root-pw-$RANDOM$RANDOM"
SVCPW="svc-pw-$RANDOM$RANDOM"
# Production's root has a password (sp-p1z81): set one before anything else runs.
timeout 5 "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "ALTER USER 'root'@'localhost' IDENTIFIED BY '$ROOTPW'" >/dev/null 2>&1 \
    || bail "could not give the fixture root a password"
root_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "$ROOTPW" --no-tls "$@"; } # batch-job: fixture SQL against the suite's private server

# spira-lc is the tree under test's own build, found by name on the suite's PATH (sp-gypjk).
command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
# Hermetic: no operator credential file can stand in for an empty SPIRA_LC_PASSWORD_FILE.
export XDG_CONFIG_HOME="$TMP/xdg"
mkdir -p "$XDG_CONFIG_HOME"
# admin-migrate's own connection is the lifecycle service user, as lc-serve's is.
printf '%s\n' "$SVCPW" > "$TMP/svc.cred"; chmod 600 "$TMP/svc.cred"
export SPIRA_LC_USER=spira_lc
tl_config SPIRA_LC_PASSWORD_FILE="$TMP/svc.cred"
unset SPIRA_LC_PASSWORD
# The admin a pending migration is applied as; cases that test its absence unset these.
export SPIRA_LC_ADMIN_USER=root
export SPIRA_LC_ADMIN_PASSWORD="$ROOTPW"
unset SPIRA_LC_ADMIN_PASSWORD_FILE
# as_root: no credential FILE can stand in for root (hermetic). SPIRA_LC_PASSWORD_FILE is a
# registered key (read only via SPIRA_TOML now, never env), so it is flipped to empty for
# this one call and restored to the service credential right after — every OTHER spira-lc
# call in this file runs outside as_root and needs the real svc.cred back in force.
as_root() {
    tl_config SPIRA_LC_PASSWORD_FILE=""
    env SPIRA_LC_USER=root SPIRA_LC_PASSWORD="$ROOTPW" "$@"
    local _rc=$?
    tl_config SPIRA_LC_PASSWORD_FILE="$TMP/svc.cred"
    return "$_rc"
}
no_admin() { env -u SPIRA_LC_ADMIN_USER -u SPIRA_LC_ADMIN_PASSWORD "$@"; }
sed -e "s/@SPIRA_LC_PASSWORD@/$SVCPW/" -e "s/@SPIRA_LC_RO_PASSWORD@/ro-$SVCPW/" "$REPO/lifecycle/grants.sql" > "$TMP/grants.sql"


columns() { root_sql --use-db spira_lifecycle sql -q "SHOW COLUMNS FROM bead" -r csv 2>/dev/null; }
reset_db() { root_sql sql -q "DROP DATABASE IF EXISTS spira_lifecycle" >/dev/null 2>&1; }
# store_from <schema-file>: a fresh spira_lifecycle from that schema, plus grants.sql.
store_from() {
    reset_db
    as_root spira-lc admin-apply-ddl "$1" >"$TMP/schema.log" 2>&1 || bail "schema did not apply: $(cat "$TMP/schema.log")"
    as_root spira-lc admin-apply-ddl "$TMP/grants.sql" >"$TMP/grants.log" 2>&1 || bail "grants did not apply: $(cat "$TMP/grants.log")"
}
SHIPPED="$REPO/lifecycle/migrations"
# The live database before round 268: schema.sql as it stood without bead.since.
pre_since_db() {
    sed '/^-- The ops read model;/,$d' "$REPO/lifecycle/schema.sql" | grep -v -e '^    since ' -e bead_state_since_idx > "$TMP/pre-since.sql"
    store_from "$TMP/pre-since.sql"
}

echo
echo "a fresh database (schema.sql on an empty server): every shipped migration is moot"
reset_db
as_root spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema.sql applies with no spira_lifecycle yet (admin-apply-ddl falls back to no database)" 0 $?
as_root spira-lc admin-apply-ddl "$TMP/grants.sql" >"$TMP/grants.log" 2>&1
wantrc "grants.sql creates the service user" 0 $?
out="$(spira-lc admin-migrate --if-enforced "$SHIPPED" 2>&1)"; rc=$?
wantrc "admin-migrate on a fresh database succeeds" 0 $rc
want "and skips 0002's column, which schema.sql already made" "0002-since.sql: bead.since present" "$out"

echo
echo "every migration applied: the service user alone decides it, no admin credential (sp-p1z81)"
printf 'SELECT 1;\n' > "$TMP/select.sql"
out="$(as_root env SPIRA_LC_PASSWORD="" SPIRA_LC_ADMIN_PASSWORD="" spira-lc admin-apply-ddl "$TMP/select.sql" 2>&1)"
want "positive control: root with an empty password is refused here, as in production" "Access denied" "$out"
out="$(no_admin spira-lc admin-migrate --if-enforced "$SHIPPED" 2>&1)"; rc=$?
wantrc "admin-migrate with only the service user succeeds" 0 $rc
want "and says no admin was needed" "every migration already applied" "$out"
want "0003's guarded UPDATE is read as applied, not re-run" "0003-terminal-holder.sql: no row left for it to change" "$out"

echo
echo "0003 pending with only the service user: guarded DML on a table it writes is applied by it (sp-p1z81)"
holders() { root_sql --use-db spira_lifecycle sql -q "SELECT bead_id, COALESCE(holder, 'none') AS h FROM bead WHERE bead_id = 'fx-terminal'" -r csv 2>/dev/null; }
root_sql --use-db spira_lifecycle sql -q "INSERT INTO bead (bead_id, state, holder, lease_until, holds, version, updated_at) VALUES ('fx-terminal', 'LANDED', 'aeon-fixture', 123, '[]', 1, 1)" >/dev/null 2>&1 \
    || bail "could not seed a terminal row that still has a holder"
want "positive control: the terminal row still carries a holder" "fx-terminal,aeon-fixture" "$(holders)"
out="$(no_admin spira-lc admin-migrate --if-enforced "$SHIPPED" 2>&1)"; rc=$?
wantrc "admin-migrate with no admin credential succeeds" 0 $rc
want "0003 applied as the service user" "0003-terminal-holder.sql: applied as the lifecycle service user" "$out"
nowant "no admin was asked for" "SPIRA_LC_ADMIN_USER" "$out"
want "the holder is gone" "fx-terminal,none" "$(holders)"
out="$(no_admin spira-lc admin-migrate --if-enforced "$SHIPPED" 2>&1)"; rc=$?
wantrc "a second run succeeds" 0 $rc
want "and finds nothing pending" "every migration already applied" "$out"
nowant "never a credential in the output" "$SVCPW" "$out"

echo
echo "a pending migration with no admin credential refuses, naming the exit (sp-p1z81)"
pre_since_db
out="$(no_admin spira-lc admin-migrate --if-enforced "$SHIPPED" 2>&1)"; rc=$?
wantrc "refused" 2 $rc
want "names SPIRA_LC_ADMIN_USER" "SPIRA_LC_ADMIN_USER" "$out"
want "names SPIRA_LC_ADMIN_PASSWORD" "SPIRA_LC_ADMIN_PASSWORD" "$out"
want "names the pending migration" "0002-since.sql is pending" "$out"
nowant "nothing applied" "since" "$(columns)"
out="$(SPIRA_LC_ADMIN_PASSWORD="wrong-$ROOTPW" spira-lc admin-migrate --if-enforced "$SHIPPED" 2>&1)"; rc=$?
wantrc "a refused admin credential refuses too" 2 $rc
want "naming the same exit" "check SPIRA_LC_ADMIN_USER and SPIRA_LC_ADMIN_PASSWORD" "$out"
nowant "and never the password it tried" "$ROOTPW" "$out"
nowant "nor the service password" "$SVCPW" "$out"
nowant "nothing applied" "since" "$(columns)"

echo
echo "a pending migration is applied before activation, and not re-run after"
pre_since_db
nowant "positive control: the pre-since store lacks since" "since" "$(columns)"
out="$(spira-lc admin-migrate --if-enforced "$SHIPPED" 2>&1)"; rc=$?
wantrc "pending migration applies, as the admin" 0 $rc
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
as_root spira-lc admin-apply-ddl "$SHIPPED/0002-since.sql" >"$TMP/m2.log" 2>&1
wantrc "0002-since.sql applies via admin-apply-ddl" 0 $?
want "since now exists" "since" "$(columns)"

echo
echo "0008 adds the per-key history index as the admin, and the history read's plan uses it"
as_root spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema2.log" 2>&1
root_sql --use-db spira_lifecycle sql -q "DROP INDEX event_history_idx ON event" >/dev/null 2>&1
hq="EXPLAIN FORMAT=TREE SELECT seq, machine, lc_key, event FROM event WHERE lc_key = 'sp-1' ORDER BY seq"
plan="$(root_sql --use-db spira_lifecycle sql -r csv -q "$hq" 2>&1)"
nowant "positive control: without the index the plan does not use it" "index: [event.lc_key,event.machine]" "$plan"
out="$(spira-lc admin-migrate --if-enforced "$SHIPPED" 2>&1)"; rc=$?
wantrc "the pending index migration applies as the admin" 0 $rc
want "reports 0008 applied" "0008-event-history-idx.sql" "$out"
plan="$(root_sql --use-db spira_lifecycle sql -r csv -q "$hq" 2>&1)"
want "the history read's plan uses the (lc_key, machine) index" "index: [event.lc_key,event.machine]" "$plan"

tl_summary
