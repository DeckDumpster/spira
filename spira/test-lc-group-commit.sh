#!/usr/bin/env bash
#
# test-lc-group-commit.sh — the lifecycle service owns the store's connections (sp-yvyvzd):
# concurrent writes through `spira-lc serve` commit together, a read through it answers what a
# direct read answers, and a declared service that is down fails the call naming it instead
# of quietly opening a connection of its own.
#
# WHAT THIS PROVES, against a throwaway `dolt sql-server` and a `spira-lc serve` this suite
# starts and tears down itself:
#   - N concurrent writers commit in FEWER than N transactions (group commit), and every
#     writer still gets its own answer: all N claims applied, and two writers racing one row's
#     version inside the burst yield exactly one applied and one refused (exit 3);
#   - `show` through the service returns what `show` in declared direct mode returns;
#   - with the service down, a write fails with exit 2 naming lc-serve and the row is
#     untouched — POSITIVE CONTROL: the same write in declared direct mode applies.
#
# A slow disk is what makes writers pile up behind a commit in production. This suite makes
# the real server slow on demand instead of modelling it: the dolt process is stopped
# (SIGSTOP) while the writers arrive and continued (SIGCONT) once they have, which is a commit
# that takes a second. Nothing is timed or asserted about time (law-no-wall-clock-budgets-in-
# the-corpus); the assertion is the count of transactions the service reports.
#
# host-reason: starts its own disposable `dolt sql-server` as a background process, the same
# shape test-lifecycle-container.sh uses inside the container testenv provides.
#
# defect: sp-yvyvzd
# tier: T2
# covers: spira-lc/src/db.rs spira-lc/src/group.rs spira-lc/src/serve.rs spira-lc/src/client.rs
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

. "$HERE/conf.sh"
export PATH="$(dirname "$DOLT_BIN"):$PATH"
unset SPIRA_LC_SOCKET

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((SPIRA_LC_TESTDB_PORT + 500 + (RANDOM % 400)))
SERVER_PID=""
SERVE_PID=""

cleanup() {
    [ -n "$SERVER_PID" ] && { kill -CONT "$SERVER_PID" $(pgrep -P "$SERVER_PID" 2>/dev/null) >/dev/null 2>&1; kill "$SERVER_PID" >/dev/null 2>&1; }
    [ -n "$SERVE_PID" ] && kill "$SERVE_PID" >/dev/null 2>&1
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

root_sql() { timeout 5 "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls --use-db spira_lifecycle "$@"; }
dolt_pids() { echo "$SERVER_PID" $(pgrep -P "$SERVER_PID" 2>/dev/null); }

BIN="$(command -v spira-lc 2>/dev/null)"; [ -n "$BIN" ] || bail "spira-lc is not on PATH (the tree's build provides it)"

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
# Registered keys, declared: no credential file (root has no password here), and direct mode
# (an empty socket) until the service is started below.
tl_config SPIRA_LC_PASSWORD_FILE="" SPIRA_LC_SOCKET=""

"$BIN" admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1 || bail "the schema did not apply: $(cat "$TMP/schema.log")"

N=16
seed=""
for i in $(seq 1 "$N"); do seed="$seed('sp-gc-$i','READY','[]',0,0),"; done
root_sql sql -q "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES ${seed}('sp-gc-race','READY','[]',0,0),('sp-gc-warm','READY','[]',0,0),('sp-gc-down','READY','[]',0,0),('sp-gc-direct','READY','[]',0,0)" >/dev/null 2>&1
wantrc "beads seeded" 0 $?

claim() {   # claim <bead-id> <holder> — one Claim from version 0; prints spira-lc's answer
    "$BIN" event bead "$1" --expect READY --version 0 --actor gc --kind "{\"Claim\":{\"holder\":\"$2\",\"lease_until\":1}}" 2>&1
}
writes_of() {   # writes_of <stats-json> <field>
    printf '%s' "$1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["writes"][sys.argv[1]])' "$2" 2>/dev/null
}

# ── the service, on a private socket ──────────────────────────────────────────────────
SOCK="$TMP/lc.sock"
"$BIN" serve "$SOCK" >"$TMP/serve.log" 2>&1 & # batch-job: the service under test, killed below and by teardown
SERVE_PID=$!
for _ in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.1; done
[ -S "$SOCK" ] || bail "spira-lc serve never created its socket: $(cat "$TMP/serve.log")"
tl_config SPIRA_LC_SOCKET="$SOCK"

# One write first, so the burst below starts from a connected write session.
out="$(claim sp-gc-warm warm)"; wantrc "a write through the service applies" 0 $?
before="$("$BIN" stats 2>&1)"
w0="$(writes_of "$before" writes)"; c0="$(writes_of "$before" commits)"
counted=1
[ -n "$w0" ] && [ -n "$c0" ] || { counted=0; bad "the service reports the writes it sent and the transactions they committed in" "no write counters in: $before"; }

# ── the burst: N writers, plus two racing one row, while a commit is slow ─────────────
kill -STOP $(dolt_pids) 2>/dev/null
for i in $(seq 1 "$N"); do
    ( claim "sp-gc-$i" "w$i" >"$TMP/w$i.out"; echo $? >"$TMP/w$i.rc" ) &
done
( claim sp-gc-race racer-a >"$TMP/ra.out"; echo $? >"$TMP/ra.rc" ) &
( claim sp-gc-race racer-b >"$TMP/rb.out"; echo $? >"$TMP/rb.rc" ) &
# Every writer is a separate process that must reach the socket before the commit finishes.
sleep 1
kill -CONT $(dolt_pids) 2>/dev/null
wait $(jobs -p | grep -vx "$SERVER_PID" | grep -vx "$SERVE_PID") 2>/dev/null

applied=0
for i in $(seq 1 "$N"); do [ "$(cat "$TMP/w$i.rc" 2>/dev/null)" = 0 ] && applied=$((applied + 1)); done
is "every one of the $N writers got its own success" "$N" "$applied"
[ "$applied" = "$N" ] || for i in $(seq 1 "$N"); do echo "# w$i rc=$(cat "$TMP/w$i.rc" 2>/dev/null): $(cat "$TMP/w$i.out" 2>/dev/null)" >&2; done
is "two writers racing one row's version: exactly one applied, one refused" "0 3" "$(sort -n "$TMP/ra.rc" "$TMP/rb.rc" | tr '\n' ' ' | sed 's/ $//')"

rows="$(root_sql sql -q "SELECT COUNT(*) AS n FROM event WHERE actor='gc' AND applied=1 AND lc_key LIKE 'sp-gc-%'" -r json 2>&1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["rows"][0]["n"])' 2>&1)"
is "the store holds every applied claim (N + race winner + warm-up)" "$((N + 2))" "$rows"

after="$("$BIN" stats 2>&1)"
if [ "$counted" = 1 ]; then
    writes=$(( $(writes_of "$after" writes) - w0 ))
    commits=$(( $(writes_of "$after" commits) - c0 ))
    echo "# the burst: $writes writes in $commits transactions (stats: $after)" >&2
    is "the service counted every write of the burst" "$((N + 2))" "$writes"
    if [ "$commits" -ge 1 ] && [ "$commits" -lt "$writes" ]; then
        ok "group commit: $writes concurrent writes committed in $commits transactions (fewer than $writes)"
    else
        bad "group commit: $writes concurrent writes committed in $commits transactions, not fewer"
    fi
fi

# ── a read through the service answers what a direct read answers ────────────────────
via="$("$BIN" show sp-gc-1 2>&1)"; via_rc=$?
tl_config SPIRA_LC_SOCKET=""
direct="$("$BIN" show sp-gc-1 2>&1)"; direct_rc=$?
tl_config SPIRA_LC_SOCKET="$SOCK"
is "show through the service exits as a direct show does" "$direct_rc" "$via_rc"
want "POSITIVE CONTROL: the direct read sees the claim" '"state": "WORKING"' "$direct"
same="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1]) == json.loads(sys.argv[2]))' "$via" "$direct" 2>&1)"
is "show through the service returns what a direct read returns" "True" "$same"
[ "$same" = True ] || echo "# via:    $via"$'\n'"# direct: $direct" >&2

# ── the service down: no fallback ─────────────────────────────────────────────────────
kill "$SERVE_PID" >/dev/null 2>&1; wait "$SERVE_PID" 2>/dev/null; SERVE_PID=""
out="$(claim sp-gc-down down)"; rc=$?
is "with the declared service down, a write exits 2 (cannot tell)" 2 "$rc"
want "the failure names the service" "lc-serve" "$out"
want "the failure names its socket" "$SOCK" "$out"
row="$(root_sql sql -q "SELECT version FROM bead WHERE bead_id='sp-gc-down'" -r json 2>&1)"
want "nothing fell back: the row is untouched" '"version":"0"' "$(printf '%s' "$row" | tr -d ' ')"
tl_config SPIRA_LC_SOCKET=""
out="$(claim sp-gc-direct direct)"; wantrc "POSITIVE CONTROL: the same write in declared direct mode applies" 0 $?

tl_summary
