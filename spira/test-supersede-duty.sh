#!/usr/bin/env bash
#
# test-supersede-duty.sh — a supersede-request is adjudicated within one pass, against a real
# spira-lc on a throwaway Dolt server: successor LANDED ends the bead SUPERSEDED; successor
# not landed (READY, or no row) ends it unheld, each with a note naming the evidence; an
# operator hold that is not a supersede-request is left alone.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-lc-hold.sh.
# tier: T1
# covers: spira/supersede-duty.sh spira/watchers spira-lc/src/callers.rs lifecycle/src/bead.rs
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"
command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"
export PATH="$PATH:$(dirname "$DOLT_BIN")"
unset SPIRA_LC_SOCKET

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
: > "$TMP/credential"
tl_config SPIRA_LC_PASSWORD_FILE="$TMP/credential" SPIRA_LC_SOCKET=""
PORT=$((23309 + 1100 + (RANDOM % 300)))
SERVER_PID=""
trap '[ -n "$SERVER_PID" ] && kill "$SERVER_PID" >/dev/null 2>&1; rm -rf "$TMP"' EXIT INT TERM

mkdir -p "$TMP/data" "$TMP/bin" "$TMP/run"
cat > "$TMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $PORT
  max_connections: 50
data_dir: "$TMP/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
"$DOLT_BIN" sql-server --config "$TMP/server.yaml" > "$TMP/server.log" 2>&1 &
SERVER_PID=$!
up=0
for _ in $(seq 1 50); do
    "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1 && { up=1; break; }
    sleep 0.2
done
[ "$up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/server.log")"
root_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls "$@"; }

export SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$PORT" SPIRA_LC_DB=spira_lifecycle \
    SPIRA_LC_DATA_DIR="$TMP" SPIRA_LC_USER=root SPIRA_LC_PASSWORD=""
spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?

seed() { root_sql --use-db spira_lifecycle sql -q "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES ('$1','$2','[]',0,0)" >/dev/null 2>&1; }
request() {   # request <bead> <successor>: the event `work superseded-by` applies
    spira-lc event bead "$1" --expect READY --version 0 --actor aeon \
        --kind "{\"Hold\":{\"kind\":\"Operator\",\"cause\":\"supersede-request\",\"detail\":\"$2\"}}" >/dev/null 2>&1
}
state_of() { spira-lc state "$1" 2>/dev/null; }

# bdq and bd record their argv; the suite asserts the note and the bd-side supersede.
for b in bdq bd; do
    printf '#!/usr/bin/env bash\necho "%s $*" >> "%s/calls.log"\n' "$b" "$TMP" > "$TMP/bin/$b"; chmod +x "$TMP/bin/$b"
done
PATH="$TMP/bin:$PATH"

seed sp-land READY;   seed sp-landed-succ LANDED
seed sp-ready READY;  seed sp-ready-succ READY
seed sp-norow READY
seed sp-manual READY
request sp-land sp-landed-succ
request sp-ready sp-ready-succ
request sp-norow sp-absent
spira-lc hold sp-manual operator "operator parked it" tester >/dev/null 2>&1

run() {
    tl_config SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/db" SPIRA_BD="$TMP/bin/bd" SPIRA_CONCIERGE_INBOX="$TMP/inbox.log"
    env -i PATH="$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" SPIRA_REPO="$REPO" \
        SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$PORT" SPIRA_LC_DB=spira_lifecycle \
        SPIRA_LC_DATA_DIR="$TMP" SPIRA_LC_USER=root SPIRA_LC_PASSWORD="" \
        bash "$HERE/supersede-duty.sh" "$@" 2>&1
}

# Positive control: the matcher sees a request when one is there.
is "the planted request is held by the operator hold" operator "$(spira-lc holds sp-land)"

out="$(run pass)"
is "successor LANDED: the bead ends SUPERSEDED" SUPERSEDED "$(state_of sp-land)"
is "the superseded row names its successor" sp-landed-succ "$(root_sql --use-db spira_lifecycle sql -q "SELECT reason FROM bead WHERE bead_id='sp-land'" -r csv 2>/dev/null | tail -1)"
want "the bd side is superseded too" "bd -C $TMP/db supersede sp-land --with sp-landed-succ" "$(cat "$TMP/calls.log")"
want "the decision is noted with the evidence" "bdq note sp-land supersede-request confirmed: sp-landed-succ is LANDED" "$(cat "$TMP/calls.log")"

is "successor READY: the bead is not superseded" READY "$(state_of sp-ready)"
is "successor READY: the operator hold is lifted" "" "$(spira-lc holds sp-ready)"
want "the refusal is noted with the successor's state" "bdq note sp-ready supersede-request by sp-ready-succ refused: sp-ready-succ is READY" "$(cat "$TMP/calls.log")"

is "successor without a row: the bead is unheld" "" "$(spira-lc holds sp-norow)"
is "successor without a row: the bead is not superseded" READY "$(state_of sp-norow)"

is "a manual operator hold is left alone" operator "$(spira-lc holds sp-manual)"
nowant "a manual hold is never adjudicated" "sp-manual" "$out"
want "each request is surfaced in the log" "SUPERSEDE REQUEST sp-ready: successor sp-ready-succ" "$out"
want "each request is surfaced to the Concierge" "[watch:supersede-duty] SUPERSEDE REQUEST sp-land" "$(cat "$TMP/inbox.log")"

: > "$TMP/calls.log"
run pass >/dev/null
is "a second pass finds nothing left to decide" "" "$(cat "$TMP/calls.log")"

run watch --interval 7 --ticks 1 >/dev/null
run health >/dev/null; is "health passes after a fresh poll" "0" "$?"
echo "ok 1 7" > "$TMP/run/watchd/supersede-duty.health"
run health >/dev/null; is "health fails once the last poll is stale" "1" "$?"

tl_summary
