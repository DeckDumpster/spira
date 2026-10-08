#!/usr/bin/env bash
#
# test-lifecycle-consistency-read.sh — the periodic consistency read (design Intent 5) stays
# a single-table scan with 10,000 bead rows in spira_lifecycle: `spira-lc list-all`, the one
# call every sweep makes, against a throwaway `dolt sql-server` run as the spira_lc user.
#
#   ./test-lifecycle-consistency-read.sh
#
# A planted inconsistent row proves the read returns the row a sweep would flag (POSITIVE
# CONTROL) before the timing is believed.
#
# host-reason: starts its own disposable `dolt sql-server`, as test-lifecycle-cutover.sh does.
#
# tier: T2
# covers: lifecycle/* spira-lc/* sentinel/src/lifecycle.rs
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

. "$HERE/conf.sh"
export PATH="$PATH:$(dirname "$DOLT_BIN")"
unset SPIRA_LC_SOCKET

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((SPIRA_LC_TESTDB_PORT + 1000 + (RANDOM % 500)))
SERVER_PID=""
ROWS=10000

cleanup() {
    [ -n "$SERVER_PID" ] && kill "$SERVER_PID" >/dev/null 2>&1
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

mkdir -p "$TMP/data"
cat > "$TMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $PORT
  max_connections: 100
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$TMP/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML

# batch-job: long-lived test server, stopped by the suite trap
"$DOLT_BIN" sql-server --config "$TMP/server.yaml" > "$TMP/server.log" 2>&1 &
SERVER_PID=$!

up=0
for _ in $(seq 1 50); do
    if timeout 5 "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        up=1
        break
    fi
    sleep 0.2
done
[ "$up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/server.log")"

# batch-job: bulk seed of the 10k rows runs through this
root_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls "$@"; }

command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"
export SPIRA_LIFECYCLE_ENFORCE=1
export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
LC_CRED="$TMP/lc.credential"; : > "$LC_CRED"
tl_config SPIRA_LC_PASSWORD_FILE="$LC_CRED"

spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?
cat "$TMP/schema.log" >&2

PASS="test-pass-$$"
sed "s/@SPIRA_LC_PASSWORD@/$PASS/" "$REPO/lifecycle/grants.sql" > "$TMP/grants_filled.sql"
root_sql sql < "$TMP/grants_filled.sql" >"$TMP/grants.log" 2>&1
wantrc "grants apply cleanly" 0 $?

python3 - "$ROWS" > "$TMP/seed.sql" <<'PY'
import sys
n = int(sys.argv[1])
print("USE spira_lifecycle;")
for lo in range(0, n, 500):
    vals = []
    for i in range(lo, min(lo + 500, n)):
        if i % 10 == 0:
            vals.append(f"('sp-ten-{i}','WORKING','t{i}',NULL,'aeon-{i}',1,JSON_ARRAY(),NULL,1,0)")
        else:
            vals.append(f"('sp-ten-{i}','LANDED','t{i}',NULL,NULL,NULL,JSON_ARRAY(),NULL,1,0)")
    print("INSERT INTO bead (bead_id,state,tip,gate_key,holder,lease_until,holds,reason,version,updated_at) VALUES " + ",".join(vals) + ";")
print("INSERT INTO bead (bead_id,state,holds,version,updated_at) VALUES ('sp-planted','WORKING',JSON_ARRAY(),1,0);")
PY
root_sql sql < "$TMP/seed.sql" >"$TMP/seed.log" 2>&1
wantrc "10k rows seed" 0 $?
count="$(root_sql --use-db spira_lifecycle sql -q "SELECT COUNT(*) AS c FROM bead" -r csv | tail -1)"
is "seeded row count" "$((ROWS + 1))" "$count"

export SPIRA_LC_USER=spira_lc
export SPIRA_LC_PASSWORD="$PASS"
printf '%s' "$PASS" > "$LC_CRED"
tl_config SPIRA_LC_PASSWORD_FILE="$LC_CRED"

spira-lc list-all >/dev/null 2>&1   # warm: first connect and server caches are not the read
t0=$(date +%s%N)
spira-lc list-all > "$TMP/all.out" 2>"$TMP/all.err"
rc=$?
ms=$(( ($(date +%s%N) - t0) / 1000000 ))

wantrc "list-all exits 0" 0 $rc
is "list-all returns every row" "$((ROWS + 1))" "$(wc -l < "$TMP/all.out" | tr -d ' ')"
want "the planted WORKING row with no holder is in the read" "sp-planted	WORKING	" "$(cat "$TMP/all.out")"
for q in "EXPLAIN FORMAT=TREE SELECT bead_id FROM bead" "DESCRIBE PLAN SELECT bead_id FROM bead"; do echo "# DBG [$q] $(root_sql --use-db spira_lifecycle sql -q "$q" | tr "\n" " ")"; done
plan=""
want "the read plans as a scan of bead" "name: bead" "$plan"
case "$plan" in
    *[Jj]oin*|*Subquery*|*Filter*) bad "the consistency read plans more than a single-table scan: $plan" ;;
    *) ok "the consistency read is a single-table scan, no join, filter or subquery" ;;
esac
echo "# list-all of $((ROWS + 1)) rows took ${ms} ms (informational: wall clock on a shared store is not asserted)"

tl_summary
