#!/usr/bin/env bash
# testlib/lc-dolt-fixture.sh — a throwaway spira_lifecycle database behind a private dolt
# sql-server, for the suites whose subject is a verb that still reads the dolt lifecycle
# database (the attempt-history facts, the journal, the ops views). Every bead and batch record
# is the control store's: testlib/lc-fixture.sh.
#
# Sourced, never executed, by a suite that already set TMP:
#   . "$HERE/testlib/lc-dolt-fixture.sh"
#   lcdolt_up || exit 1            # starts the server, applies the schema, exports SPIRA_LC_*
#   env -i SPIRA_RELEASE="$(tl_release)" ... $(lcdolt_env) spira-lc facts ...   # carry the connection through an `env -i`
#
# lcdolt_up_on_testdb instead puts the database on a server-mode testdb's own server, for a
# suite whose subject reads the beads through spira-lc (plan items, ask titles,
# reconcile-content): spira-lc reads the planning store over the lifecycle connection, so it
# sees the beads only when both live on one server, as they do in production.
#
# lcdolt_down is idempotent; call it from the suite's EXIT trap.
LCDOLT_PID=""
LCDOLT_DIR=""

lcdolt_sql() { "$LCDOLT_DOLT" --data-dir "$LCDOLT_DIR" --host 127.0.0.1 --port "$SPIRA_LC_PORT" -u root -p "" --no-tls --use-db spira_lifecycle sql "$@"; }

lcdolt_up() {
    LCDOLT_DOLT="$(command -v dolt 2>/dev/null)" || { echo "lc-dolt-fixture: dolt not on PATH" >&2; return 1; }
    command -v spira-lc >/dev/null 2>&1 || { echo "lc-dolt-fixture: spira-lc not on PATH" >&2; return 1; }
    LCDOLT_DIR="$(mktemp -d)"
    mkdir -p "$LCDOLT_DIR/data"
    local port=$((23000 + (RANDOM % 9000))) _
    cat > "$LCDOLT_DIR/server.yaml" <<YAML
log_level: warning
listener:
  port: $port
  max_connections: 100
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$LCDOLT_DIR/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
    "$LCDOLT_DOLT" sql-server --config "$LCDOLT_DIR/server.yaml" > "$LCDOLT_DIR/server.log" 2>&1 &
    LCDOLT_PID=$!
    unset SPIRA_LC_SOCKET
    # The registered half of the connection is declared in this suite's own config layer
    # (the one source): the fixture's own (empty-password) credential file, and no socket.
    : > "$LCDOLT_DIR/credential"
    tl_config SPIRA_LC_PASSWORD_FILE="$LCDOLT_DIR/credential" SPIRA_LC_SOCKET="" \
        || { echo "lc-dolt-fixture: cannot declare the lifecycle connection" >&2; return 1; }
    export SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$port" SPIRA_LC_DB=spira_lifecycle \
        SPIRA_LC_DATA_DIR="$LCDOLT_DIR" SPIRA_LC_USER=root SPIRA_LC_PASSWORD=""
    local up=0
    for _ in $(seq 1 100); do
        if "$LCDOLT_DOLT" --data-dir "$LCDOLT_DIR" --host 127.0.0.1 --port "$port" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
            up=1; break
        fi
        sleep 0.1
    done
    [ "$up" = 1 ] || { echo "lc-dolt-fixture: dolt sql-server never came up: $(cat "$LCDOLT_DIR/server.log")" >&2; return 1; }
    local ddl="${LCDOLT_SCHEMA:-$HERE/../lifecycle/schema.sql}"
    spira-lc admin-apply-ddl "$ddl" > "$LCDOLT_DIR/schema.log" 2>&1 \
        || { echo "lc-dolt-fixture: schema failed: $(cat "$LCDOLT_DIR/schema.log")" >&2; return 1; }
}

lcdolt_up_on_testdb() {
    LCDOLT_DOLT="$(command -v dolt 2>/dev/null)" || { echo "lc-dolt-fixture: dolt not on PATH" >&2; return 1; }
    command -v spira-lc >/dev/null 2>&1 || { echo "lc-dolt-fixture: spira-lc not on PATH" >&2; return 1; }
    [ "${TESTDB_MODE:-}" = server ] || { echo "lc-dolt-fixture: lcdolt_up_on_testdb needs a server-mode testdb (SPIRA_TESTDB_MODE=server)" >&2; return 1; }
    LCDOLT_DIR="$(mktemp -d)"
    unset SPIRA_LC_SOCKET
    : > "$LCDOLT_DIR/credential"
    tl_config SPIRA_LC_PASSWORD_FILE="$LCDOLT_DIR/credential" SPIRA_LC_SOCKET="" \
        || { echo "lc-dolt-fixture: cannot declare the lifecycle connection" >&2; return 1; }
    export SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_DB=spira_lifecycle \
        SPIRA_LC_DATA_DIR="$LCDOLT_DIR" SPIRA_LC_USER=root SPIRA_LC_PASSWORD=""
    _lcdolt_schema_on_testdb || return 1
    [ -n "${_LCDOLT_ON_TESTDB:-}" ] && return 0
    _LCDOLT_ON_TESTDB=1
    # A testdb_reset restarts that server on a fresh copy of the bead template, which holds no
    # lifecycle database: the schema follows every reset, so each section starts on empty stores.
    eval "$(declare -f testdb_reset | sed '1s/testdb_reset/_lcdolt_testdb_reset_store/')"
    testdb_reset() { _lcdolt_testdb_reset_store "$@" && _lcdolt_schema_on_testdb; }
}

_lcdolt_schema_on_testdb() {
    local port
    port="$(cat "$SPIRA_DB/.beads/dolt-server.port" 2>/dev/null)" \
        || { echo "lc-dolt-fixture: the testdb names no server port" >&2; return 1; }
    export SPIRA_LC_PORT="$port"
    spira-lc admin-apply-ddl "${LCDOLT_SCHEMA:-$HERE/../lifecycle/schema.sql}" > "$LCDOLT_DIR/schema.log" 2>&1 \
        || { echo "lc-dolt-fixture: schema failed: $(cat "$LCDOLT_DIR/schema.log")" >&2; return 1; }
}

lcdolt_down() {
    [ -n "$LCDOLT_PID" ] && kill "$LCDOLT_PID" >/dev/null 2>&1
    [ -n "$LCDOLT_PID" ] && wait "$LCDOLT_PID" 2>/dev/null
    LCDOLT_PID=""
    [ -n "$LCDOLT_DIR" ] && rm -rf "$LCDOLT_DIR"
    LCDOLT_DIR=""
}

# lcdolt_reset_facts — forget every attempt-history fact; a suite that resets its bd fixture
# between sections resets these with it (lcdolt_follow_testdb).
lcdolt_reset_facts() { lcdolt_sql -q "DELETE FROM event WHERE machine='fact'" >/dev/null 2>&1; }

# lcdolt_fact_causes <id> <kind> — the cause of each fact of that kind, one per line, oldest first.
lcdolt_fact_causes() {
    spira-lc facts --ids "$1" --kinds "$2" 2>/dev/null \
        | python3 -c 'import json, sys; [print(r["new_value"]) for r in json.load(sys.stdin)]'
}

# lcdolt_follow_testdb — from here on, testdb_reset also clears the facts.
lcdolt_follow_testdb() {
    eval "$(declare -f testdb_reset | sed '1s/testdb_reset/_lcdolt_testdb_reset_bd/')"
    testdb_reset() { _lcdolt_testdb_reset_bd "$@" && lcdolt_reset_facts; }
}

lcdolt_env() {
    printf 'SPIRA_TOML=%s SPIRA_LC_HOST=%s SPIRA_LC_PORT=%s SPIRA_LC_DB=%s SPIRA_LC_DATA_DIR=%s SPIRA_LC_USER=%s SPIRA_LC_PASSWORD=' \
        "$SPIRA_TOML" "$SPIRA_LC_HOST" "$SPIRA_LC_PORT" "$SPIRA_LC_DB" "$SPIRA_LC_DATA_DIR" "$SPIRA_LC_USER"
}
