#!/usr/bin/env bash
#
# test-store-persist.sh — a Dolt store served from a RAM directory survives losing that
# directory: seeded from a stopped store, restored before its server starts, synced by the
# daemon while a writer streams autocommit rows, then the server is SIGKILLed mid-stream and
# the directory wiped (a host crash). Restored, every row acknowledged before the newest
# finished pass began is present and the survivors are a prefix of the stream; a clean stop
# with the stop-time pass loses nothing. The pass-by-pass mechanics are store-persist's
# cargo tests; this suite proves the real binary against a real server.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-refusal-watch.sh.
#
# tier: T2
# covers: store-persist/* systemd/dolt-beads.service systemd/dolt-persist.* spira/conf.d/SPIRA_DOLT_DURABLE spira/conf.d/SPIRA_DOLT_PERSIST_*
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

command -v dolt >/dev/null 2>&1 || bail "dolt is not on PATH"
command -v store-persist >/dev/null 2>&1 || bail "store-persist is not on PATH"

TMP="$(mktemp -d)"
SERVER="" DAEMON="" WRITER=""
cleanup() {
    for p in $WRITER $DAEMON $SERVER; do kill -9 "$p" 2>/dev/null; done
    wait 2>/dev/null
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

OLD="$TMP/old" RAM="$TMP/ram" DURABLE="$TMP/durable"
PORT=$((23000 + (RANDOM % 9000)))
unset SPIRA_DOLT_DATA SPIRA_DOLT_DURABLE SPIRA_DOLT_PERSIST_INTERVAL_SECS SPIRA_DOLT_PERSIST_MAX_LAG_SECS SPIRA_DOLT_PERSIST_COMPACT_PCT
tl_config SPIRA_DOLT_DATA="$RAM" SPIRA_DOLT_DURABLE="$DURABLE" SPIRA_DOLT_PERSIST_INTERVAL_SECS=1 \
    SPIRA_DOLT_PERSIST_MAX_LAG_SECS=30 SPIRA_DOLT_PERSIST_COMPACT_PCT=175 \
    || bail "cannot declare the store-persist config"

sql() { dolt --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls --use-db "$1" sql -r csv -q "$2"; }

server_up() {
    ( cd "$RAM" && exec dolt sql-server --config "$RAM/dolt-server.yaml" ) > "$TMP/server.log" 2>&1 &
    SERVER=$!
    local _
    for _ in $(seq 1 150); do
        sql probe "SELECT 1" >/dev/null 2>&1 && return 0
        sleep 0.1
    done
    bail "fixture dolt sql-server never came up: $(tail -5 "$TMP/server.log")"
}

# The stopped store the cutover starts from: two databases, rows uncommitted in the working
# set as the server's dolt_transaction_commit=false leaves them.
for db in probe other; do
    mkdir -p "$OLD/$db" && ( cd "$OLD/$db" && dolt init --name fixture --email fixture@localhost >/dev/null ) \
        || bail "cannot init $db"
done
( cd "$OLD/probe" && dolt sql -q "CREATE TABLE w (seq INT PRIMARY KEY, at_ms BIGINT); INSERT INTO w VALUES (0, 0);" ) \
    || bail "cannot create the stream table"

out="$(store-persist seed --from "$OLD" 2>&1)"
wantrc "seed builds the durable copy from the stopped store" 0 $?
want "seed names both databases" "other,probe" "$out"

# install places the server config in the (RAM) data directory.
mkdir -p "$RAM"
cat > "$RAM/dolt-server.yaml" <<YAML
log_level: warning
listener:
  port: $PORT
data_dir: "$RAM"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML

out="$(store-persist restore 2>&1)"
wantrc "restore fills the empty RAM directory" 0 $?
want "restore reports both databases restored" "restored [other,probe]" "$out"
is "the git remote cache is a link to disk" "$DURABLE/git-remote-cache/probe" "$(readlink "$RAM/probe/.dolt/git-remote-cache")"

server_up
is "the uncommitted seed row came through the copy" "1" "$(sql probe "SELECT COUNT(*) AS n FROM w" | tail -1)"

store-persist restore >/dev/null 2>&1
wantrc "restore refuses to run under a live server" 1 $?
store-persist pass --offline >/dev/null 2>&1
wantrc "an offline pass refuses a live server" 1 $?

store-persist run >> "$TMP/daemon.log" 2>&1 &
DAEMON=$!

# The writer: one autocommit insert at a time, its acknowledgement logged after it returns.
writer() {
    local i=1
    while :; do
        sql probe "INSERT INTO w VALUES ($i, $(date +%s%3N))" >/dev/null 2>&1 || return 0
        echo "$i $(date +%s%3N)" >> "$TMP/acks"
        i=$((i + 1))
    done
}
writer &
WRITER=$!

for _ in $(seq 1 120); do
    [ "$(wc -l < "$TMP/acks" 2>/dev/null || echo 0)" -ge 30 ] && break
    sleep 0.1
done
seed_start="$(sed -n 's/^start=//p' "$DURABLE/persist.state")"

# The host crash: the server dies mid-stream, then the daemon, then RAM is gone.
kill -9 "$SERVER"; wait "$SERVER" 2>/dev/null; SERVER=""
crash_ms="$(date +%s%3N)"
kill -9 "$DAEMON"; wait "$DAEMON" 2>/dev/null; DAEMON=""
wait "$WRITER" 2>/dev/null; WRITER=""
acked="$(wc -l < "$TMP/acks")"
[ "$acked" -ge 5 ] || bail "the writer acknowledged only $acked rows before the crash"
start="$(sed -n 's/^start=//p' "$DURABLE/persist.state")"
[ "$start" -gt "$seed_start" ] && ok "the daemon finished passes during the stream" \
    || bad "the daemon finished passes during the stream" "persist.state start $start, seed $seed_start; $(tail -3 "$TMP/daemon.log")"
rm -rf "$RAM"/* "$RAM"/.dolt "$RAM"/.doltcfg

out="$(store-persist restore 2>&1)"
wantrc "restore after the crash" 0 $?
want "the server config came back from the copy" "dolt-server.yaml" "$out"
server_up
survivors="$(sql probe "SELECT seq FROM w WHERE seq > 0 ORDER BY seq" | tail -n +2)"
n="$(printf '%s\n' "$survivors" | grep -c .)"
max="$(printf '%s\n' "$survivors" | tail -1)"
is "the survivors are a prefix of the stream" "$(seq 1 "${max:-0}" | tr '\n' ' ')" "$(printf '%s\n' "$survivors" | tr '\n' ' ')"
lost_before="$(awk -v s="$start" -v m="${max:-0}" '$1 > m && $2 < s * 1000 { n++ } END { print n + 0 }' "$TMP/acks")"
is "no row acknowledged before the newest pass began is lost" 0 "$lost_before"
[ "$n" -ge 1 ] && ok "rows streamed after the seed survived the crash" || bad "rows streamed after the seed survived the crash" "0 of $acked"
echo "# crash: $acked acknowledged, $n restored, $((acked - n)) lost; newest pass began $(( crash_ms - start * 1000 )) ms before the kill"

store-persist age >/dev/null 2>&1
wantrc "age is green inside the bound" 0 $?

# A planned stop: SIGTERM, then the stop-time pass the unit runs as ExecStopPost.
sql probe "INSERT INTO w VALUES (100000, 0)" >/dev/null 2>&1 || bad "write after restore" "$(tail -3 "$TMP/server.log")"
kill "$SERVER"; wait "$SERVER" 2>/dev/null; SERVER=""
store-persist pass --offline >/dev/null 2>"$TMP/offline.err"
wantrc "the stop-time pass runs offline" 0 $?
rm -rf "$RAM"/* "$RAM"/.dolt "$RAM"/.doltcfg
store-persist restore >/dev/null 2>&1 || bad "restore after a planned stop" "$(cat "$TMP/offline.err")"
server_up
is "a planned stop loses nothing" "1" "$(sql probe "SELECT COUNT(*) AS n FROM w WHERE seq = 100000" | tail -1)"

tl_summary
