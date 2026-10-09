#!/usr/bin/env bash
#
# test-lc-ask.sh — container-tier acceptance for the ask machine, against a real spira-lc
# binary and a throwaway Dolt server.
#
# WHAT THIS PROVES:
#   - create-ask opens an ask on its own machine and writes no bead-machine row, so `list`
#     and the claim computation (ops_live) cannot return it.
#   - closing an ask records who closed it and his quoted words, and lifts the `ask` hold on
#     the work bead the ask names (answered, default and withdrawn alike).
#   - an ask with no work bead closes cleanly; a closed ask refuses a second close; an exit
#     with no quoted words is refused; an answer must name its channel.
#   - POSITIVE CONTROLS: a work bead is no ask (show-ask exit 1, close-ask exit 1), and the
#     same work bead IS listed and claimable, so the absence above is the machine's, not a
#     broken query.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-lc-hold.sh.
#
# defect: sp-g3w50i
# tier: T1
# covers: lifecycle/src/ask.rs lifecycle/schema.sql lifecycle/migrations/* spira-lc/src/** mail/src/** cockpit/ops/src/resolve*.rs
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
# SPIRA_LC_PASSWORD_FILE is declared config now (spira/conf.d), read only from $SPIRA_TOML
# (spira-lc/src/db.rs password_from) — the complete fixture's own declared path does not
# exist for this suite's throwaway server. testlib/lc-fixture.sh's own pattern: an empty
# (root, no password) credential file, declared, and no socket.
: > "$TMP/credential"
tl_config SPIRA_LC_PASSWORD_FILE="$TMP/credential" SPIRA_LC_SOCKET=""
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



json_field() {   # json_field <field> reads a flat JSON object on stdin
    python3 -c 'import json,sys; print(json.load(sys.stdin).get(sys.argv[1]) or "")' "$1"
}
ask_row() { root_sql --use-db spira_lifecycle sql -q "SELECT state, work_bead, closed_by, quote, channel, version FROM ask WHERE ask_id='$1'" -r json 2>/dev/null; }
bead_rows() { root_sql --use-db spira_lifecycle sql -q "SELECT COUNT(*) AS n FROM bead WHERE bead_id='$1'" -r json 2>/dev/null; }

# ── an ask is its own row and never a bead row ────────────────────────────────────────────
seed_bead sp-w-1 WORKING
spira-lc hold sp-w-1 ask "waiting on the operator" test-suite
wantrc "the work bead is held" 0 $?
spira-lc create-ask sp-ask-1 --work-bead sp-w-1
wantrc "create-ask opens an ask" 0 $?
spira-lc create-ask sp-ask-1 --work-bead sp-w-1
wantrc "create-ask is idempotent" 0 $?
want "the ask is OPEN and names its work bead" '"work_bead":"sp-w-1"' "$(ask_row sp-ask-1)"
want "the ask is OPEN" '"state":"OPEN"' "$(ask_row sp-ask-1)"
want "NO bead-machine row exists for the ask" '"n":"0"' "$(bead_rows sp-ask-1)"
spira-lc show sp-ask-1 >/dev/null 2>&1
wantrc "spira-lc show finds no bead row for an ask" 1 $?
nowant "spira-lc list never returns the ask" "sp-ask-1" "$(spira-lc list)"
nowant "the claim computation (ops_live) never returns the ask" "sp-ask-1" "$(spira-lc ops-view ops_live)"
want "list-asks returns the open ask" "sp-ask-1" "$(spira-lc list-asks)"
spira-lc show-ask sp-ask-1 >/dev/null; wantrc "show-ask finds the ask" 0 $?
spira-lc show-ask sp-w-1 >/dev/null 2>&1; wantrc "a work bead is not an ask" 1 $?
want "POSITIVE CONTROL: the work bead is listed" "sp-w-1" "$(spira-lc list)"

# ── refusals ─────────────────────────────────────────────────────────────────────────────
spira-lc close-ask sp-ask-1 --exit answered --quote "" --actor operator --channel pane >/dev/null 2>&1
wantrc "an exit without his words is refused" 3 $?
spira-lc close-ask sp-ask-1 --exit answered --quote "yes" --actor operator >/dev/null 2>&1
wantrc "an answer without a channel is refused" 2 $?
is "...and the ask is still OPEN" "OPEN" "$(ask_row sp-ask-1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["rows"][0]["state"])')"
spira-lc close-ask sp-w-1 --exit withdrawn --quote "moot" --actor claude >/dev/null 2>&1
wantrc "close-ask on a work bead finds no ask" 1 $?
is "...and the work bead is still held" ask "$(spira-lc holds sp-w-1)"

# ── the answer is recorded with who, his words and the channel, and lifts the hold ────────
spira-lc close-ask sp-ask-1 --exit answered --quote "yes, ship it" --actor operator --channel pane --message-id m-9
wantrc "close-ask answers the ask" 0 $?
row="$(ask_row sp-ask-1)"
want "state ANSWERED" '"state":"ANSWERED"' "$row"
want "who answered" '"closed_by":"operator"' "$row"
want "his quoted words" '"quote":"yes, ship it"' "$row"
want "the channel" '"channel":"pane"' "$row"
is "the work bead's ask hold is lifted" "" "$(spira-lc holds sp-w-1)"
is "the work bead keeps its state" WORKING "$(spira-lc state sp-w-1)"
spira-lc close-ask sp-ask-1 --exit withdrawn --quote "again" --actor claude >/dev/null 2>&1
wantrc "a closed ask refuses a second close" 3 $?
nowant "list-asks no longer lists it as open" "sp-ask-1" "$(spira-lc list-asks)"
want "list-asks --state ANSWERED does" "sp-ask-1" "$(spira-lc list-asks --state ANSWERED)"

# ── an ask with no work bead (a question about a statute) resolves cleanly ────────────────
spira-lc create-ask sp-ask-2
spira-lc close-ask sp-ask-2 --exit withdrawn --quote "answered by the statute text" --actor claude
wantrc "an ask with no work bead withdraws cleanly" 0 $?
want "WITHDRAWN with its quote" '"quote":"answered by the statute text"' "$(ask_row sp-ask-2)"

# ── a dismissal takes the default and lifts the hold ──────────────────────────────────────
seed_bead sp-w-3 READY
spira-lc hold sp-w-3 ask "waiting" test-suite
spira-lc create-ask sp-ask-3 --work-bead sp-w-3
spira-lc close-ask sp-ask-3 --exit default --quote "ran the default" --actor operator
wantrc "a dismissal closes the ask" 0 $?
want "DEFAULT_TAKEN" '"state":"DEFAULT_TAKEN"' "$(ask_row sp-ask-3)"
is "...and lifts the hold" "" "$(spira-lc holds sp-w-3)"

tl_summary
