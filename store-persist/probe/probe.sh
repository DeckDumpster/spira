#!/usr/bin/env bash
# probe.sh — measurements behind store-persist/DESIGN.md, against fixture dolt servers only.
#
#   probe.sh up <data-dir> <port>            start a fixture sql-server (root, no password)
#   probe.sh down <data-dir>                 SIGTERM it
#   probe.sh kill9 <data-dir>                SIGKILL it
#   probe.sh seed <data-dir> <db>            create <db> with table w(seq, at)
#   probe.sh latency <port> <db> <n>         n autocommit inserts in one session; prints ms/txn
#   probe.sh sync <data-dir> <db> <dest>     one CALL dolt_backup('sync-url') through the server; prints ms
#   probe.sh restore <backup> <parent> <db>  `dolt backup restore`; prints ms
#   probe.sh offline <dir> <db> <sql>        query a database with no server
#   probe.sh trace-sync <dir> <db> <dest> <out>   strace an offline sync's fsync/rename order
#   probe.sh local-tcp <data-dir> <db> <sql> query as the server's local superuser over TCP
#   probe.sh q <data-dir> <db> <sql>         query from inside the database directory
#
# A production port, data directory or credential is never an input: every server here is one
# this script started, in a directory it was handed.
set -uo pipefail
DOLT="${DOLT:-dolt}"

_cli() { local port="$1" db="$2"; shift 2; "$DOLT" --host 127.0.0.1 --port "$port" -u root -p "" --no-tls --use-db "$db" sql "$@"; }

cmd="${1:-}"; shift || true
case "$cmd" in
up)
    dir="$1" port="$2"
    mkdir -p "$dir/data"
    cat > "$dir/server.yaml" <<YAML
log_level: warning
listener:
  port: $port
  max_connections: 100
data_dir: "$dir/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
system_variables:
  dolt_stats_enabled: 0
YAML
    ( cd "$dir/data" && exec "$DOLT" sql-server --config "$dir/server.yaml" ) > "$dir/server.log" 2>&1 &
    echo $! > "$dir/server.pid"
    for _ in $(seq 1 200); do
        "$DOLT" --host 127.0.0.1 --port "$port" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1 && { echo "up pid $(cat "$dir/server.pid")"; exit 0; }
        sleep 0.1
    done
    echo "never came up: $(tail -5 "$dir/server.log")" >&2; exit 1 ;;
down)
    pid="$(cat "$1/server.pid")"; kill "$pid" 2>/dev/null; while kill -0 "$pid" 2>/dev/null; do sleep 0.1; done ;;
kill9)
    pid="$(cat "$1/server.pid")"; kill -9 "$pid" 2>/dev/null; while kill -0 "$pid" 2>/dev/null; do sleep 0.05; done ;;
seed)
    dir="$1" db="$2"
    mkdir -p "$dir/data/$db" && ( cd "$dir/data/$db" && "$DOLT" init --name probe --email probe@localhost >/dev/null ) || exit 1 ;;
latency)
    port="$1" db="$2" n="$3"
    _cli "$port" "$db" -q "CREATE TABLE IF NOT EXISTS lat (seq INT PRIMARY KEY, pad VARCHAR(200))" || exit 1
    base="$(_cli "$port" "$db" -r csv -q "SELECT COALESCE(MAX(seq),0) AS m FROM lat" | sed -n 2p)"
    sqlf="$(mktemp)"
    for i in $(seq 1 "$n"); do printf "INSERT INTO lat VALUES (%d, REPEAT('x',200));\n" $((base + i)); done > "$sqlf"
    t0="$(date +%s%N)"
    _cli "$port" "$db" < "$sqlf" >/dev/null || { rm -f "$sqlf"; exit 1; }
    t1="$(date +%s%N)"; rm -f "$sqlf"
    echo "$n txns in $(( (t1 - t0) / 1000000 )) ms = $(( (t1 - t0) / 1000 / n )) us/txn" ;;
sync)
    dir="$1" db="$2" dest="$3"
    t0="$(date +%s%N)"
    out="$(cd "$dir/data/$db" && echo "CALL DOLT_BACKUP('sync-url', 'file://$dest');" | "$DOLT" sql 2>&1)" || { echo "sync failed: $out" >&2; exit 1; }
    t1="$(date +%s%N)"
    echo "sync $(( (t1 - t0) / 1000000 )) ms" ;;
restore)
    src="$1" parent="$2" db="$3"
    mkdir -p "$parent" && t0="$(date +%s%N)" && ( cd "$parent" && "$DOLT" backup restore "file://$src" "$db" ) || exit 1
    echo "restore $(( ($(date +%s%N) - t0) / 1000000 )) ms" ;;
offline)
    dir="$1" db="$2"; shift 2
    ( cd "$dir/$db" && printf '%s\n' "$*" | "$DOLT" sql -r csv ) ;;
trace-sync)
    # Which syscalls make an offline sync durable: fsync/rename order of the backup's files.
    dir="$1" db="$2" dest="$3" out="$4"
    ( cd "$dir/$db" && echo "CALL DOLT_BACKUP('sync-url', 'file://$dest');" \
        | strace -f -o "$out" -e trace=fsync,fdatasync,sync_file_range,rename,renameat,renameat2,openat "$DOLT" sql ) || exit 1
    grep -E 'fsync|fdatasync|rename|'"$dest" "$out" | grep -v ENOENT ;;
local-tcp)
    # The server's own local superuser, over TCP: the CLI can never fall back to opening the store itself.
    dir="$1" db="$2"; shift 2
    IFS=: read -r _ port secret < "$dir/data/.dolt/sql-server.info"
    printf '%s\n' "$*" | DOLT_CLI_PASSWORD="$secret" "$DOLT" --host 127.0.0.1 --port "$port" --no-tls -u __dolt_local_user__ --use-db "$db" sql -r csv ;;
q)
    dir="$1" db="$2"; shift 2
    ( cd "$dir/data/$db" && printf '%s\n' "$*" | "$DOLT" sql -r csv ) ;;
*)
    sed -n '2,11p' "$0" >&2; exit 2 ;;
esac
