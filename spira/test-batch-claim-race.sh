#!/usr/bin/env bash
#
# test-batch-claim-race.sh — a batch landing racing an aeon's claim: the claim is refused.
#
# Against a throwaway `dolt sql-server` and the real spira-lc binary, a member bead sits IN_DELIVERY
# while the batch's `delivered` event and a crowd of aeon `claim` events all arrive at once:
#   - the landing is applied and the row ends LANDED with the merge sha as its reason;
#   - every claim is refused (exit non-zero) and none leaves a holder or a lease on the row;
#   - the version advanced by exactly one and the event log holds one applied row;
#   - POSITIVE CONTROLS: the same claim on a READY row IS applied, and the same landing on the
#     IN_DELIVERY row with no claimant IS applied, so a refusal is the lifecycle table speaking.
#   - a claim arriving after the landing (the terminal row) is refused too.
# Seen red against a planted defect: the IN_DELIVERY row accepting a Claim in lifecycle/src/bead.rs.
#
# host-reason: starts its own disposable `dolt sql-server` as a background process, the same shape
# test-lifecycle-container.sh uses
# tier: T2
# covers: lifecycle/* spira-lc/*
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin"
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH"

. "$HERE/conf.sh"
export PATH="$(dirname "$CARGO_BIN"):$(dirname "$DOLT_BIN"):$PATH"
unset SPIRA_LC_SOCKET

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((SPIRA_LC_TESTDB_PORT + (RANDOM % 500)))
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

"$DOLT_BIN" sql-server --config "$TMP/server.yaml" > "$TMP/server.log" 2>&1 &
SERVER_PID=$!

# Wait for the listener, rather than a fixed sleep: the suite must not flake on a slow box.
up=0
for _ in $(seq 1 50); do
    if "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        up=1
        break
    fi
    sleep 0.2
done
[ "$up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/server.log")"

root_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls "$@"; }
# as_user <user> <password> <dolt-args...> — a separate function, not an override appended
# after root_sql's own -u/-p, because relying on "the last -u/-p flag wins" is a guess
# about dolt's flag parser this suite has no reason to make.
as_user() {
    local u="$1" p="$2"
    shift 2
    "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u "$u" -p "$p" --no-tls "$@"
}

# PIN CARGO_TARGET_DIR EXPLICITLY (same hazard as test-batcher-cut.sh): a suite runs
# inside testenv-batch.sh's own podman exec, which sets its own CARGO_TARGET_DIR for the
# suites that build Rust under test. Trusting $REPO/target here builds into that redirected
# directory instead, and this suite's own binary lookup finds nothing there — SEEN RED
# without this pin, as "cargo build" reporting success while the lookup path stayed empty.
CARGO_TARGET_DIR_FOR_BUILD="$TMP/cargo-target"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
    "$CARGO_BIN" build --manifest-path "$REPO/spira-lc/Cargo.toml" --quiet 2>"$TMP/build.log" \
    || bail "spira-lc failed to build: $(cat "$TMP/build.log")"
BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/spira-lc"

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""

"$BIN" admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?


seed_row() {   # seed_row <bead-id> <state> <version>
    root_sql --use-db spira_lifecycle sql -q \
        "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES ('$1','$2','[]',$3,0)" >/dev/null 2>&1
}
sql_field() {  # sql_field <query> <column>
    root_sql --use-db spira_lifecycle sql -q "$1" -r json \
        | python3 -c 'import json,sys; r=json.load(sys.stdin)["rows"]; print(r[0]["'"$2"'"] if r else "")' 2>/dev/null
}
claim() {      # claim <bead-id> <expect> <version> <aeon>
    "$BIN" event bead "$1" --expect "$2" --version "$3" --actor "$4" \
        --kind "{\"Claim\":{\"holder\":\"$4\",\"lease_until\":9}}" >/dev/null 2>&1
}
land() {       # land <bead-id> <version>
    "$BIN" event bead "$1" --expect IN_DELIVERY --version "$2" --actor batch \
        --kind '{"Delivered":{"merge_sha":"abc123","proof":"batch"}}' >/dev/null 2>&1
}

# POSITIVE CONTROLS: both events do apply where the table allows them.
seed_row sp-pc-claim READY 0
claim sp-pc-claim READY 0 aeon-pc
wantrc "control: a claim on a READY row is applied" 0 $?
seed_row sp-pc-land IN_DELIVERY 5
land sp-pc-land 5
wantrc "control: a landing on an IN_DELIVERY row is applied" 0 $?
is "control: the landed row is LANDED" LANDED "$(sql_field "SELECT state FROM bead WHERE bead_id='sp-pc-land'" state)"

# A claim alone on a delivering row is refused.
seed_row sp-alone IN_DELIVERY 5
claim sp-alone IN_DELIVERY 5 aeon-alone
rc=$?
[ "$rc" != 0 ] && ok "a claim on an IN_DELIVERY row is refused" || bad "a claim on an IN_DELIVERY row is refused" "applied"
is "the refused claim left no holder" "" "$(sql_field "SELECT holder FROM bead WHERE bead_id='sp-alone'" holder)"

# The race: one landing against N claims, all holding the same read of the row.
seed_row sp-race IN_DELIVERY 5
N=8
pids=()
( land sp-race 5; echo $? > "$TMP/land.rc" ) &
pids+=("$!")
for i in $(seq 1 "$N"); do
    ( claim sp-race IN_DELIVERY 5 "aeon-$i"; echo $? > "$TMP/claim-$i.rc" ) &
    pids+=("$!")
done
wait "${pids[@]}"

is "the landing is applied" 0 "$(cat "$TMP/land.rc")"
claims_applied=0
for i in $(seq 1 "$N"); do
    [ "$(cat "$TMP/claim-$i.rc")" = 0 ] && claims_applied=$((claims_applied + 1))
done
is "no racing claim is applied" 0 "$claims_applied"
is "the row ends LANDED" LANDED "$(sql_field "SELECT state FROM bead WHERE bead_id='sp-race'" state)"
is "the landing's merge sha is the row's reason" abc123 "$(sql_field "SELECT reason FROM bead WHERE bead_id='sp-race'" reason)"
is "no claimant holds the landed row" "" "$(sql_field "SELECT holder FROM bead WHERE bead_id='sp-race'" holder)"
is "the version advanced by exactly one" 6 "$(sql_field "SELECT version FROM bead WHERE bead_id='sp-race'" version)"
is "exactly one event on the row was applied" 1 "$(sql_field "SELECT COUNT(*) AS n FROM event WHERE lc_key='sp-race' AND applied=1" n)"

# A claim that arrives after the landing finds a terminal row.
claim sp-race LANDED 6 aeon-late
rc=$?
[ "$rc" != 0 ] && ok "a claim after the landing is refused" || bad "a claim after the landing is refused" "applied"
is "the late claim left no holder" "" "$(sql_field "SELECT holder FROM bead WHERE bead_id='sp-race'" holder)"

tl_summary
