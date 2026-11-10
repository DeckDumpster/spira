#!/usr/bin/env bash
#
# test-lifecycle-fresh-migrate.sh — `spira-lc admin-migrate` over every shipped migration,
# against a real `dolt sql-server`, from the two starting points an install meets.
#
#   ./test-lifecycle-fresh-migrate.sh
#
# WHAT THIS PROVES:
#   - A database schema.sql just created reaches the current schema with every migration
#     recorded as already satisfied or applied cleanly (exit 0), runtime_effect in its
#     current shape.
#   - A database at migration 0037's schema still migrates to the same current schema.
#
# host-reason: starts its own disposable `dolt sql-server`, as test-lifecycle-migrate.sh does.
#
# defect: sp-vii35h
# tier: T2
# covers: lifecycle/migrations/* lifecycle/schema.sql spira-lc/src/migrate.rs
# timeout: 180
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
PORT=$((3309 + 1400 + (RANDOM % 300)))
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
export SPIRA_LC_ADMIN_USER=root SPIRA_LC_ADMIN_PASSWORD=""
# SPIRA_LC_PASSWORD_FILE EXPLICITLY, EMPTY: it is a registered key, resolved from SPIRA_TOML
# alone now — left undeclared it falls to the complete fixture's own (nonexistent) path, and
# every spira-lc call refuses before reaching the dolt server. Empty matches root's actual
# password here; switched to the real one below once this suite moves to spira_lc.
LC_CRED="$TMP/lc.credential"; : > "$LC_CRED"
tl_config SPIRA_LC_PASSWORD_FILE="$LC_CRED"

cols() { root_sql --use-db spira_lifecycle sql -r csv -q "SELECT column_name FROM information_schema.columns WHERE table_schema = 'spira_lifecycle' AND table_name = '$1' ORDER BY ordinal_position" | tail -n +2 | paste -sd,; }
CURRENT="task,ekey,request_id,stream,spec,requested_at,deadline_at,resolved"

spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema.sql applies to an empty database" 0 $?
spira-lc admin-migrate "$REPO/lifecycle/migrations" >"$TMP/fresh.log" 2>&1
wantrc "a fresh database migrates with exit 0" 0 $?

want "0038 is skipped because its table was renamed aside" "0038-runtime-effect-open.sql: its table was renamed aside" "$(cat "$TMP/fresh.log")"
is "the fresh database has the current runtime_effect" "$CURRENT" "$(cols runtime_effect)"
is "the 0038 table survives under its renamed name" "event_id,seq,resolved" "$(cols runtime_effect_0038)"
spira-lc admin-migrate "$REPO/lifecycle/migrations" >"$TMP/again.log" 2>&1
wantrc "a second run over the migrated database is clean" 0 $?

root_sql sql -q "DROP DATABASE spira_lifecycle; CREATE DATABASE spira_lifecycle" >/dev/null 2>&1
spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema.sql applies again" 0 $?
root_sql --use-db spira_lifecycle sql -q "
DROP TABLE runtime_effect; DROP TABLE runtime_effect_0038; DROP TABLE runtime_task_state;
DROP INDEX event_gantt_idx ON event;
ALTER TABLE runtime_event MODIFY COLUMN kind_body TEXT NOT NULL;
ALTER TABLE runtime_schedule MODIFY COLUMN kind_body TEXT NOT NULL;
DROP TABLE planning_watermark;" >"$TMP/revert.log" 2>&1
wantrc "the database is cut back to what migration 0037 left" 0 $?
cat "$TMP/revert.log" >&2
spira-lc admin-migrate "$REPO/lifecycle/migrations" >"$TMP/old.log" 2>&1
wantrc "a database at 0037 migrates with exit 0" 0 $?
cat "$TMP/old.log" >&2
is "the migrated 0037 database has the current runtime_effect" "$CURRENT" "$(cols runtime_effect)"
is "...and the renamed 0038 table" "event_id,seq,resolved" "$(cols runtime_effect_0038)"

tl_summary
