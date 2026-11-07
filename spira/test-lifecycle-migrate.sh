#!/usr/bin/env bash
#
# test-lifecycle-migrate.sh — lifecycle migrations that change data, run end to end against a
# throwaway `dolt sql-server` as the real spira_lc grant set.
#
#   ./test-lifecycle-migrate.sh
#
# WHAT THIS PROVES:
#   - 0019-event-continuity corrects the hard-coded from_state of a logged Deliver (to the
#     state the bead was in) and Cut (to NONE when the delivery had no predecessor), leaves
#     the log continuous, and a second application changes nothing. POSITIVE CONTROL: the
#     uncorrected log is first seen as a break by `event-continuity`.
#
# host-reason: starts its own disposable `dolt sql-server`, the same shape every testdb.sh
# server-mode suite already uses without a container call.
#
# defect: sp-wutvrr
# tier: T2
# covers: lifecycle/migrations/* lifecycle/schema.sql spira-lc/src/continuity.rs spira-lc/src/migrate.rs
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# Resolve cargo/dolt BEFORE conf.sh — see test-lifecycle-container.sh's own note: the
# testenv image puts cargo at /usr/local/cargo/bin, not under $HOME, and conf.sh can
# overwrite PATH with the harness's own tool directories first.
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

. "$HERE/conf.sh"
export PATH="$PATH:$(dirname "$DOLT_BIN")"
unset SPIRA_LC_SOCKET

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((3309 + 1000 + (RANDOM % 300)))
SERVER_PID=""

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

"$DOLT_BIN" sql-server --config "$TMP/server.yaml" > "$TMP/server.log" 2>&1 & # batch-job: long-lived fixture listener, killed by the suite teardown
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

root_sql() { timeout 5 "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls "$@"; }

# spira-lc is the tree under test's own build, found by name on the suite's PATH (sp-gypjk).
command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
# SPIRA_LC_PASSWORD_FILE EXPLICITLY, EMPTY: it is a registered key, resolved from SPIRA_TOML
# alone now — left undeclared it falls to the complete fixture's own (nonexistent) path, and
# every spira-lc call refuses before reaching the dolt server. Empty matches root's actual
# password here; switched to the real one below once this suite moves to spira_lc.
LC_CRED="$TMP/lc.credential"; : > "$LC_CRED"
tl_config SPIRA_LC_PASSWORD_FILE="$LC_CRED"

spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
[ $? -eq 0 ] || { cat "$TMP/schema.log" >&2; bail "the lifecycle schema did not apply"; }
cat "$TMP/schema.log" >&2

PASS="test-pass-$$"
sed "s/@SPIRA_LC_PASSWORD@/$PASS/" "$REPO/lifecycle/grants.sql" > "$TMP/grants_filled.sql"
root_sql sql < "$TMP/grants_filled.sql" >"$TMP/grants.log" 2>&1
[ $? -eq 0 ] || { cat "$TMP/grants.log" >&2; bail "the lifecycle grants did not apply"; }
cat "$TMP/grants.log" >&2

# From here on, every `spira-lc` call runs AS spira_lc — the real production grant set (design
# §3.6: "only spira_lc writes spira_lifecycle"), never root. A cascade that only works with
# superuser privileges would be a false green.
export SPIRA_LC_USER=spira_lc
export SPIRA_LC_PASSWORD="$PASS"
printf '%s' "$PASS" > "$LC_CRED"
tl_config SPIRA_LC_PASSWORD_FILE="$LC_CRED"

ev_field() {   # ev_field <machine> <key> <event> <column>
    root_sql --use-db spira_lifecycle sql -q "SELECT $4 AS v FROM event WHERE machine = '$1' AND lc_key = '$2' AND event = '$3' AND applied = 1 ORDER BY seq DESC LIMIT 1" -r json \
        | python3 -c 'import json,sys; d=json.load(sys.stdin)["rows"]; print(d[0]["v"] if d else "")' 2>/dev/null
}

CONT_RUN="$TMP/continuity-run"; mkdir -p "$CONT_RUN"
cont() { SPIRA_TOML="$(tl_layer SPIRA_RUN="$CONT_RUN")" spira-lc event-continuity; }

# ── migration 0019 corrects the hard-coded Deliver/Cut states from their predecessors ───
root_sql --use-db spira_lifecycle sql -q "INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, evidence, actor, at) VALUES ('bead','sp-lc-mig','Submit','WORKING','WORKING','SUBMITTED',1,'{}','t',1),('bead','sp-lc-mig','Deliver','CERTIFIED','CERTIFIED','IN_DELIVERY',1,'{}','t',2),('delivery','sp-lc-mig','Cut','QUEUED','QUEUED','BATCHED',1,'{}','t',3)" >/dev/null
cont >/dev/null
wantrc "POSITIVE CONTROL: the old hard-coded states are seen as breaks" 3 $?
root_sql --use-db spira_lifecycle sql < "$REPO/lifecycle/migrations/0019-event-continuity.sql" >/dev/null 2>&1
wantrc "migration 0019 applies" 0 $?
is "the Deliver now says what the bead was" "SUBMITTED" "$(ev_field bead sp-lc-mig Deliver from_state)"
is "the Cut of a row with no predecessor says NONE" "NONE" "$(ev_field delivery sp-lc-mig Cut from_state)"
cont >/dev/null
wantrc "...and the log is continuous again" 0 $?
root_sql --use-db spira_lifecycle sql < "$REPO/lifecycle/migrations/0019-event-continuity.sql" >/dev/null 2>&1
wantrc "migration 0019 is idempotent" 0 $?

tl_summary
