#!/usr/bin/env bash
#
# test-lc-list-timing.sh — `spira-lc list` and `list --state READY` are served by the since covering index
# on a store of production size (12,000 beads, 100,000 events), against a real Dolt.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-lc-hold.sh.
#
# defect: sp-jk7xgp
# tier: T1
# covers: spira-lc/src/main.rs spira-lc/src/db.rs lifecycle/schema.sql lifecycle/migrations/*
# timeout: 240
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
# SPIRA_LC_PASSWORD_FILE is declared config now (spira/conf.d), read only from $SPIRA_TOML
# (spira-lc/src/db.rs password_from) — the complete fixture's own declared path does not
# exist for this suite's throwaway server. testlib/lc-fixture.sh's own pattern: an empty
# (root, no password) credential file, declared, and no socket.
: > "$TMP/credential"
tl_config SPIRA_LC_PASSWORD_FILE="$TMP/credential" SPIRA_LC_SOCKET=""
PORT=$((SPIRA_LC_TESTDB_PORT + 700 + (RANDOM % 300)))
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

"$DOLT_BIN" sql-server --config "$TMP/server.yaml" > "$TMP/server.log" 2>&1 & # batch-job: long-lived fixture listener, killed by the suite teardown
SERVER_PID=$!

up=0
for _ in $(seq 1 50); do
    if timeout 5 "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        up=1; break
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

spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?

seed_bead() {   # seed_bead <bead-id> <state>
    root_sql --use-db spira_lifecycle sql -q \
        "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES ('$1','$2','[]',0,0)" >/dev/null 2>&1
}
row_json() {    # row_json <bead-id>
    root_sql --use-db spira_lifecycle sql -q \
        "SELECT state, holds, version FROM bead WHERE bead_id='$1'" -r json 2>/dev/null
}



BEADS=12000
EVENTS=100000
{
    echo "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES"
    awk -v n=$BEADS 'BEGIN{for(i=0;i<n;i++) printf "%s('"'"'sp-%06d'"'"','"'"'%s'"'"','"'"'[]'"'"',1,1)", (i?",":""), i, (i%12?"LANDED":"READY")}'
    echo ";"
    awk -v n=$EVENTS -v b=$BEADS 'BEGIN{srand(7); for(c=0;c<n;c+=5000){ printf "INSERT INTO event (machine,lc_key,event,expect,from_state,to_state,applied,evidence,actor,at) VALUES"; for(i=0;i<5000;i++) printf "%s('"'"'bead'"'"','"'"'sp-%06d'"'"','"'"'e'"'"','"'"'X'"'"','"'"'A'"'"','"'"'%s'"'"',%d,'"'"'{}'"'"','"'"'a'"'"',%d)", (i?",":""), int(rand()*b), (rand()<.5?"LANDED":"READY"), (rand()<.9), c+i; print ";"}}'
} > "$TMP/seed.sql"
timeout 120 "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls --use-db spira_lifecycle sql < "$TMP/seed.sql" >/dev/null 2>&1 # batch-job: seeds a production-size fixture into the suite's private dolt
wantrc "production-size store seeds" 0 $?
is "the fixture holds the beads" "$BEADS" "$(root_sql --use-db spira_lifecycle sql -q "SELECT COUNT(*) AS n FROM bead" -r csv 2>/dev/null | tail -1)"
is "the fixture holds the events" "$EVENTS" "$(root_sql --use-db spira_lifecycle sql -q "SELECT COUNT(*) AS n FROM event" -r csv 2>/dev/null | tail -1)"

# A wall-clock budget flips under shared load; the cost is asserted on the plan instead.
plan="$(root_sql --use-db spira_lifecycle sql -r csv -q "EXPLAIN FORMAT=TREE SELECT e.lc_key, e.to_state, MAX(e.at) AS since FROM event e JOIN bead b ON b.bead_id = e.lc_key AND b.state = e.to_state WHERE e.machine = 'bead' AND e.applied = 1 GROUP BY e.lc_key, e.to_state" 2>&1; echo "rc=$?")"
case "$plan" in *"MergeJoin"*"index: [event.machine,event.applied,event.lc_key,event.to_state,event.at]"*) ok "the since join is a merge join over the event_since_idx columns" ;; *) bad "the since join is a merge join over the event_since_idx columns: $(printf %s "$plan" | tr "\n" " ")" ;; esac

for args in "list" "list --state READY"; do
    out="$(spira-lc $args 2>"$TMP/err")"
    rc=$?
    wantrc "spira-lc $args exits 0" 0 $rc
    case "$args" in
        "list") is "list returns every bead" "$BEADS" "$(printf '%s' "$out" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')" ;;
        *) is "list --state READY returns the READY beads" "$((BEADS / 12))" "$(printf '%s' "$out" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')" ;;
    esac
done

# since is populated on a bead that has an applied event into its current state
since_ok="$(spira-lc list --state READY | python3 -c 'import json,sys; print(sum(1 for b in json.load(sys.stdin) if b.get("since") is not None) > 0)')"
is "since is filled from the event log" True "$since_ok"

spira-lc list --state READY --hold poison >/dev/null 2>&1
wantrc "list with both filters is valid SQL" 0 $?

tl_summary
