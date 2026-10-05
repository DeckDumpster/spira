#!/usr/bin/env bash
# testlib/lc-fixture.sh — a throwaway spira_lifecycle database behind a private dolt
# sql-server, for suites whose subject reads or writes delivery state (the queue's one record).
#
# Sourced, never executed, by a suite that already set TMP:
#   . "$HERE/testlib/lc-fixture.sh"
#   lcfix_up || exit 1            # starts the server, applies the schema, exports SPIRA_LC_*
#   lcfix_seed sp-a CERTIFIED <tip>
#   env -i ... $(lcfix_env) queue ...   # carry the connection through an `env -i`
#   lcfix_state sp-a              # -> CERTIFIED
#
# lcfix_down is idempotent; call it from the suite's EXIT trap.
LCFIX_PID=""
LCFIX_DIR=""

lcfix_sql() { "$LCFIX_DOLT" --data-dir "$LCFIX_DIR" --host 127.0.0.1 --port "$SPIRA_LC_PORT" -u root -p "" --no-tls --use-db spira_lifecycle sql "$@"; }

lcfix_up() {
    LCFIX_DOLT="$(command -v dolt 2>/dev/null)" || { echo "lc-fixture: dolt not on PATH" >&2; return 1; }
    command -v spira-lc >/dev/null 2>&1 || { echo "lc-fixture: spira-lc not on PATH" >&2; return 1; }
    LCFIX_DIR="$(mktemp -d)"
    mkdir -p "$LCFIX_DIR/data"
    local port=$((23000 + (RANDOM % 9000))) _
    cat > "$LCFIX_DIR/server.yaml" <<YAML
log_level: warning
listener:
  port: $port
  max_connections: 100
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$LCFIX_DIR/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
    "$LCFIX_DOLT" sql-server --config "$LCFIX_DIR/server.yaml" > "$LCFIX_DIR/server.log" 2>&1 &
    LCFIX_PID=$!
    unset SPIRA_LC_SOCKET
    export SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$port" SPIRA_LC_DB=spira_lifecycle \
        SPIRA_LC_DATA_DIR="$LCFIX_DIR" SPIRA_LC_USER=root SPIRA_LC_PASSWORD=""
    local up=0
    for _ in $(seq 1 100); do
        if "$LCFIX_DOLT" --data-dir "$LCFIX_DIR" --host 127.0.0.1 --port "$port" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
            up=1; break
        fi
        sleep 0.1
    done
    [ "$up" = 1 ] || { echo "lc-fixture: dolt sql-server never came up: $(cat "$LCFIX_DIR/server.log")" >&2; return 1; }
    local ddl="${LCFIX_SCHEMA:-$HERE/../lifecycle/schema.sql}"
    spira-lc admin-apply-ddl "$ddl" > "$LCFIX_DIR/schema.log" 2>&1 \
        || { echo "lc-fixture: schema failed: $(cat "$LCFIX_DIR/schema.log")" >&2; return 1; }
}

lcfix_down() {
    [ -n "$LCFIX_PID" ] && kill "$LCFIX_PID" >/dev/null 2>&1
    [ -n "$LCFIX_PID" ] && wait "$LCFIX_PID" 2>/dev/null
    LCFIX_PID=""
    [ -n "$LCFIX_DIR" ] && rm -rf "$LCFIX_DIR"
    LCFIX_DIR=""
}

lcfix_env() {
    printf 'SPIRA_LC_HOST=%s SPIRA_LC_PORT=%s SPIRA_LC_DB=%s SPIRA_LC_DATA_DIR=%s SPIRA_LC_USER=%s SPIRA_LC_PASSWORD=' \
        "$SPIRA_LC_HOST" "$SPIRA_LC_PORT" "$SPIRA_LC_DB" "$SPIRA_LC_DATA_DIR" "$SPIRA_LC_USER"
}

# lcfix_seed <id> <STATE> [tip] [since-epoch]
# An upsert, not REPLACE: REPLACE is a DELETE plus an INSERT, and a bead a `spira-lc cut`
# already put in a batch has delivery/batch_member rows whose foreign keys refuse that
# DELETE — so a re-seed of a batched bead silently left it IN_DELIVERY. A seed that does
# not take says so on stderr and returns non-zero.
lcfix_seed() {
    local id="$1" state="$2" tip="${3:-}" since="${4:-}"
    local tipv="NULL" sincev="NULL" out
    [ -n "$tip" ] && tipv="'$tip'"
    [ -n "$since" ] && sincev="$since"
    out="$(lcfix_sql -q "INSERT INTO bead (bead_id, state, tip, holds, version, since, updated_at) VALUES ('$id','$state',$tipv,'[]',1,$sincev,0) ON DUPLICATE KEY UPDATE state=VALUES(state), tip=VALUES(tip), holds=VALUES(holds), version=version+1, since=VALUES(since), updated_at=VALUES(updated_at)" 2>&1)" \
        || { echo "lc-fixture: seeding $id $state failed: $out" >&2; return 1; }
}

lcfix_state() { lcfix_sql -q "SELECT state FROM bead WHERE bead_id='$1'" -r csv 2>/dev/null | sed -n 2p; }
lcfix_tip() { lcfix_sql -q "SELECT IFNULL(tip,'') FROM bead WHERE bead_id='$1'" -r csv 2>/dev/null | sed -n 2p; }
lcfix_reason() { lcfix_sql -q "SELECT IFNULL(reason,'') FROM bead WHERE bead_id='$1'" -r csv 2>/dev/null | sed -n 2p; }
