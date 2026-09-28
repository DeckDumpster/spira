#!/usr/bin/env bash
#
# test-unpoison.sh — unpoison.sh clears spira-poison so that CHECK 4 will not put it back.
#
# The failure this guards: clearing a poison by removing the label left the attempt count at
# the threshold, so the very next sentinel pass re-poisoned the bead (2026-09-26, six beads).
# The CONTROL case below reproduces exactly that with a label-only clear; the tool's case must
# come out the other way, judged by check4_decide — the function CHECK 4 itself calls.
#
# tier: T2
# covers: spira/unpoison.sh spira/lib.sh spira/lc.sh spira-lc/* lifecycle/*
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-unpoison.sh"
. "$HERE/testdb.sh"
testdb_available || skip "no fixture database reachable"
testdb_require unpoison
TMP="$(mktemp -d)"
# testdb-mode: server — attempts_of and the poison.cleared floor read the events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
testdb_up unpoison || skip "server testdb not available"
. "$HERE/lib.sh"
export SPIRA_POISON_ASKED="$TMP/poison-asked"; mkdir -p "$SPIRA_POISON_ASKED"

# A REAL spira-lc against a throwaway Dolt server (sp-rlyl0), so the poison hold this bead
# makes unpoison.sh release is proven against the actual machine, not assumed from lc.sh's
# own exit code. Same shape as test-lc-hold.sh.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin"
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"
unset SPIRA_LC_SOCKET

REPO="$(cd "$HERE/.." && pwd)"
LCPORT=$((SPIRA_LC_TESTDB_PORT + 900 + (RANDOM % 300)))
LC_SERVER_PID=""
trap 'testdb_drop; [ -n "$LC_SERVER_PID" ] && kill "$LC_SERVER_PID" >/dev/null 2>&1; rm -rf "$TMP"' EXIT INT TERM

mkdir -p "$TMP/lc-data"
cat > "$TMP/lc-server.yaml" <<YAML
log_level: warning
listener:
  port: $LCPORT
  max_connections: 20
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$TMP/lc-data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
"$DOLT_BIN" sql-server --config "$TMP/lc-server.yaml" > "$TMP/lc-server.log" 2>&1 &
LC_SERVER_PID=$!
lc_up=0
for _ in $(seq 1 50); do
    if "$DOLT_BIN" --data-dir "$TMP/lc-data" --host 127.0.0.1 --port "$LCPORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        lc_up=1; break
    fi
    sleep 0.2
done
[ "$lc_up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/lc-server.log")"
lc_root_sql() { "$DOLT_BIN" --data-dir "$TMP/lc-data" --host 127.0.0.1 --port "$LCPORT" -u root -p "" --no-tls "$@"; }

CARGO_TARGET_DIR_FOR_BUILD="$TMP/cargo-target"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
    "$CARGO_BIN" build --manifest-path "$REPO/spira-lc/Cargo.toml" --quiet 2>"$TMP/lc-build.log" \
    || bail "spira-lc failed to build: $(cat "$TMP/lc-build.log")"
export SPIRA_LC_BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/spira-lc"
export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$LCPORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP/lc-data"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
"$SPIRA_LC_BIN" admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/lc-schema.log" 2>&1
wantrc "spira-lc schema applies cleanly" 0 $?

. "$HERE/lc.sh"
lc_seed_working_poisoned() {   # lc_seed_working_poisoned <bead-id>
    lc_root_sql --use-db spira_lifecycle sql -q \
        "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES ('$1','WORKING','[\"poison\"]',0,0)" >/dev/null 2>&1
}

seedt() {   # seedt <id> <event_type> <new_value> <created_at>
    local uuid; uuid="$(python3 -c 'import uuid; print(uuid.uuid4())')"
    bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$1', '$2', 'harness', '$3', '$4')" >/dev/null 2>&1
}
labels_of() { bdjson show "$1" | python3 -c 'import sys,json
d=json.load(sys.stdin); b=(d if isinstance(d,list) else [d])[0]; print(",".join(b.get("labels") or []))'; }
status_of() { bdjson show "$1" | python3 -c 'import sys,json
d=json.load(sys.stdin); b=(d if isinstance(d,list) else [d])[0]; print(b.get("status",""))'; }
decide() { check4_decide "$(attempts_of "$1")" "$(requeues_of "$1")" "$(reclaims_of "$1")" "$(labels_of "$1")"; }
UNPOISON="$HERE/unpoison.sh"

testdb_reset
testdb_seed <<JSONL
{"id":"pz1","title":"poisoned by three failed claims","status":"open","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"pz2","title":"control: label-only clear","status":"open","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"pz3","title":"held by a live aeon","status":"in_progress","assignee":"aeon-test","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"pz4","title":"healthy","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"pzask","title":"Spira bead pz1 — 3 in_progress transition(s) without landing (3 attempts) — change the approach or drop it?","status":"open","issue_type":"decision","labels":["$SPIRA_ASK_LABEL","overseer"],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
for id in pz1 pz2 pz3; do
    for t in '2026-09-01 01:00:00' '2026-09-01 02:00:00' '2026-09-01 03:00:00'; do seedt "$id" claimed '' "$t"; done
done
lc_seed_working_poisoned pz1
lc_seed_working_poisoned pz3

echo
echo "CONTROL — removing only the label leaves CHECK 4 about to re-poison:"
bdq label remove pz2 spira-poison >/dev/null 2>&1
want "label-only clear: check4 still decides poison" "poison" "$(decide pz2)"

echo
echo "unpoison.sh clears so it sticks:"
out="$(bash "$UNPOISON" --bead pz1 --cause "every session ended its turn 'waiting for' a background batch" 2>&1)"; rc=$?
is   "exit 0" "0" "$rc"
want "reports OK" "OK   pz1" "$out"
# THE LABEL IS NOT unpoison.sh's TO REMOVE (sp-rlyl0): it belongs to CHECK 4, which adds it
# on poisoning and stale-clears it on its own next pass once attempts falls back below
# threshold — this fixture never runs CHECK 4, so the label legitimately stays. What
# unpoison.sh guarantees directly is the floored attempt count and check4_decide's verdict.
want "label is left for CHECK 4's own stale-clear to remove" "spira-poison" "$(labels_of pz1)"
is   "attempt count floored to 0" "0" "$(attempts_of pz1)"
nowant "check4 no longer decides poison" "poison" "$(decide pz1)"
nowant "the lifecycle poison hold is released, against a real spira-lc" "poison" "$(lc_holds pz1)"
is   "...and the lifecycle row's WORKING state is undisturbed by releasing the hold" \
     "WORKING" "$(_lc_json_field "$(lc_show pz1)" 'd.get("bead",{}).get("state","")')"
is   "the poisoning's operator ask is resolved" "closed" "$(status_of pzask)"
want "the cause is recorded on the bead" "every session ended its turn" "$(bdjson show pz1 | python3 -c 'import sys,json
d=json.load(sys.stdin); b=(d if isinstance(d,list) else [d])[0]; print(b.get("notes") or "")')"

echo
echo "refusals:"
out="$(bash "$UNPOISON" --bead pz1 2>&1)"; is "no --cause is a usage error" "2" "$?"
out="$(bash "$UNPOISON" --bead pz3 --cause x 2>&1)"; rc=$?
is   "a bead a live aeon holds is refused" "1" "$rc"
want "and says who holds it" "held by aeon-test" "$out"
want "and leaves its poison" "spira-poison" "$(labels_of pz3)"
want "and leaves its lifecycle poison hold too, against a real spira-lc" "poison" "$(lc_holds pz3)"
out="$(bash "$UNPOISON" --bead pz4 --cause x 2>&1)"; rc=$?
is   "a healthy bead is skipped, not an error" "0" "$rc"
want "and says so" "SKIP pz4" "$out"
out="$(bash "$UNPOISON" pz1 --cause x 2>&1)"; is "a positional bead id is refused" "2" "$?"

tl_summary
