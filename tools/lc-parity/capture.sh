#!/usr/bin/env bash
# capture.sh — spira-lc's answers on the dolt path, as golden data a port proves parity against.
#
#   capture.sh [--source DATA_DIR | --save-seed DIR] [--spira-lc BIN] [--cases FILE] [--work DIR] OUT_DIR
#
# Without --source it builds the seed testdb itself (the cases file's `seed`). With one, it
# copies that data directory, which must hold a stopped spira_lifecycle store. Every case runs
# on its own copy of that copy, behind a private dolt sql-server, so a case's golden depends on
# the seed and its own steps and nothing else. OUT_DIR gets, per case and step,
# `<case>/<NN>.argv` (one argument per line), `.stdin` when given, `.stdout`, `.stderr`, `.rc`.
# The seed's own calls are recorded under `_seed/` the same way, so another backend can replay it.
#
# Deterministic by construction: SPIRA_NOW pins the clock; the private store's path and port are
# written as @STORE@ and @PORT@, and a harness log line's wall-clock stamp as @WALL@.
#
# It refuses a source, work directory or output that is, contains, or sits inside the store the
# caller's config declares (spira.dolt_data, spira.dolt_durable), or a store a server is running on.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$HERE/../.." && pwd -P)"
NOW_EPOCH=1767225600

die() { echo "lc-parity: $*" >&2; exit 2; }

source_dir="" save_seed="" lc_bin="spira-lc" cases="$HERE/cases.sh" out="" work=""
while [ $# -gt 0 ]; do
    case "$1" in
        --source) source_dir="${2:-}"; shift 2 ;;
        --spira-lc) lc_bin="${2:-}"; shift 2 ;;
        --cases) cases="${2:-}"; shift 2 ;;
        --work) work="${2:-}"; shift 2 ;;
        --save-seed) save_seed="${2:-}"; shift 2 ;;
        -*) die "unknown option $1" ;;
        *) [ -z "$out" ] || die "one OUT_DIR only"; out="$1"; shift ;;
    esac
done
[ -n "$out" ] || die "usage: capture.sh [--source DATA_DIR | --save-seed DIR] [--spira-lc BIN] [--cases FILE] [--work DIR] OUT_DIR"
lc_bin="$(command -v "$lc_bin")" || die "spira-lc not found"
DOLT="$(command -v dolt)" || die "dolt not on PATH"
[ -f "$cases" ] || die "no cases file at $cases"

# The production store, as the caller's own config declares it. A config that cannot be read
# fails closed: a guard that cannot see the live path cannot tell a copy of it from a testdb.
[ -n "${SPIRA_TOML:-}" ] || die "SPIRA_TOML is not set — cannot tell where the production store is; refusing"
protected=()
for key in spira.dolt_data spira.dolt_durable; do
    v="$(spira-config get "$key" 2>"${TMPDIR:-/tmp}/lc-parity-conf.$$")"; rc=$?
    err="$(cat "${TMPDIR:-/tmp}/lc-parity-conf.$$" 2>/dev/null)"; rm -f "${TMPDIR:-/tmp}/lc-parity-conf.$$"
    # An undeclared key fails silently; an unreadable config says why, and is a refusal.
    [ "$rc" -eq 0 ] || [ -z "$err" ] || die "cannot read $key from SPIRA_TOML: $err"
    if [ "$rc" -ne 0 ] && [ "$key" = spira.dolt_data ]; then
        v="${XDG_DATA_HOME:-$HOME/.local/share}/spira/dolt"
    fi
    [ -n "$v" ] && protected+=("$(realpath -m "$v")")
done
[ ${#protected[@]} -gt 0 ] || die "the config names no production store path; refusing"

overlaps() {  # overlaps <a> <b> — a is b, inside b, or contains b
    case "$1/" in "$2"/*) return 0 ;; esac
    case "$2/" in "$1"/*) return 0 ;; esac
    return 1
}
guard() {  # guard <what> <path>
    local p; p="$(realpath -m "$2")"
    for live in "${protected[@]}"; do
        overlaps "$p" "$live" && die "$1 $2 overlaps the production store $live; refusing"
    done
}

# A directory this tool may empty: absent, empty, or one it wrote before (it carries the marker).
MARK=.lc-parity
ours() { [ ! -e "$1" ] || [ -f "$1/$MARK" ] || [ -z "$(ls -A "$1" 2>/dev/null)" ]; }
guard OUT_DIR "$out"
ours "$out" || die "OUT_DIR $out is neither empty nor an earlier capture (no $MARK); refusing to empty it"
[ -z "$work" ] || ours "$work" || die "the work directory $work is neither empty nor an earlier one; refusing"
[ -z "$work" ] || guard "the work directory" "$work"
[ -z "$save_seed" ] || { [ -z "$source_dir" ] || die "--save-seed seeds a fresh testdb; it takes no --source"; guard "the seed copy" "$save_seed"; }
if [ -n "$source_dir" ]; then
    guard "the source" "$source_dir"
    [ -d "$source_dir/spira_lifecycle/.dolt" ] || die "$source_dir holds no spira_lifecycle store"
    # A link inside the copy would reach whatever it points at, the live store included.
    [ -z "$(find "$source_dir" -type l 2>/dev/null | head -1)" ] || die "$source_dir holds a symbolic link; copy only a plain store"
    if [ -n "$(find "$source_dir" -maxdepth 3 -name sql-server.lock 2>/dev/null | head -1)" ]; then
        die "a server is running on $source_dir; copy only a stopped store"
    fi
fi
made_work=0
if [ -z "$work" ]; then
    work="$(mktemp -d "${TMPDIR:-/tmp}/lc-parity.XXXXXX")" || die "cannot make a work directory"
    made_work=1
fi
guard "the work directory" "$work"
mkdir -p "$work" || die "cannot create $work"
: > "$work/$MARK"
work="$(realpath "$work")"

SERVER_PID="" STORE="" PORT=""
cleanup() { stop_server; [ "$made_work" = 1 ] && [ "${LC_PARITY_KEEP:-0}" != 1 ] && rm -rf "$work"; }
trap cleanup EXIT

stop_server() {
    [ -n "$SERVER_PID" ] || return 0
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
    SERVER_PID=""
}

# The config every spira-lc call reads: the complete fixture, then this layer. Direct mode (no
# socket), so nothing can be forwarded to a running service.
layer="$work/layer.toml"
printf '[spira]\n' > "$layer"
: > "$work/credential"
for kv in "lc_socket=" "lc_password_file=$work/credential" "run=$work/run"; do
    SPIRA_HOME="$REPO/spira" spira-config set "spira.${kv%%=*}" "${kv#*=}" "$layer" >/dev/null \
        || die "cannot declare ${kv%%=*}"
done
mkdir -p "$work/run" "$work/home" "$work/bin"
CHILD_TOML="$REPO/spira/testlib/complete.toml:$layer"

root_sql() {
    "$DOLT" --data-dir "$STORE" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql "$@" # STORE_DOOR_CONSIDERED="the tool's own private server, never the lifecycle store"
}

# start_server <data-dir>: a private sql-server on a free port. It is ours only when the
# marker database this run planted in the data directory answers; anything else on that port
# is somebody else's server and is never written to.
start_server() {
    STORE="$1"
    local marker="lcparity_$$_$RANDOM" tries _
    mkdir -p "$STORE/$marker" || die "cannot plant the marker database"
    (cd "$STORE/$marker" && "$DOLT" init --name lc-parity --email lc-parity@localhost >/dev/null 2>&1) || die "cannot plant the marker database" # STORE_DOOR_CONSIDERED="a marker in the tool's own copy"
    for tries in 1 2 3 4 5; do
        PORT=$((23000 + (RANDOM % 9000)))
        cat > "$STORE/../server.yaml" <<YAML
log_level: warning
listener:
  host: 127.0.0.1
  port: $PORT
  max_connections: 100
data_dir: "$STORE"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
        "$DOLT" sql-server --config "$STORE/../server.yaml" > "$STORE/../server.log" 2>&1 & # STORE_DOOR_CONSIDERED="the tool's own private server on its own copy"
        SERVER_PID=$!
        for _ in $(seq 1 150); do
            kill -0 "$SERVER_PID" 2>/dev/null || break
            if root_sql -q "SHOW DATABASES" -r csv 2>/dev/null | grep -qx "$marker"; then
                rm -rf "${STORE:?}/$marker"
                root_sql -q "DROP DATABASE IF EXISTS $marker" >/dev/null 2>&1
                return 0
            fi
            sleep 0.1
        done
        stop_server
    done
    die "no private sql-server came up on $STORE: $(tail -5 "$STORE/../server.log")"
}

# Steps write here; `lc`/`lc_in` are what the cases file calls.
STEP_DIR="" STEP_N=0 NOW="$NOW_EPOCH"
now() { NOW="$1"; }

# An empty PATH: whatever a verb shells out to (bd, git) is absent on every host alike.
run_lc() {  # run_lc <stdin-file-or-empty> args...
    local in="$1"; shift
    env -i PATH="$work/bin" HOME="$work/home" LC_ALL=C TZ=UTC \
        SPIRA_TOML="$CHILD_TOML" SPIRA_HOME="$REPO/spira" SPIRA_NOW="$NOW" \
        SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$PORT" SPIRA_LC_DB=spira_lifecycle \
        SPIRA_LC_DATA_DIR="$STORE" SPIRA_LC_USER=root SPIRA_LC_PASSWORD= \
        "$lc_bin" "$@" < "${in:-/dev/null}"
}

# A harness log line (`<UTC stamp> spira: ...`) is stamped from the wall clock, not SPIRA_NOW.
re() { printf '%s' "$1" | sed -e 's/[][\.*^$#+?(){}|]/\\&/g'; }  # a path, as an ERE matching only itself
normalize() {
    sed -E -e "s#$(re "$STORE")#@STORE@#g" -e "s#$(re "$work")#@WORK@#g" -e "s#:$PORT\\b#:@PORT@#g" \
        -e 's#^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z spira: #@WALL@ spira: #'
}

record() {  # record <stdin-text-or-empty> args...
    local text="$1"; shift
    STEP_N=$((STEP_N + 1))
    local base; base="$STEP_DIR/$(printf '%02d' "$STEP_N")"
    local in=""
    printf '%s\n' "$@" > "$base.argv"
    [ "$NOW" = "$NOW_EPOCH" ] || printf '%s\n' "$NOW" > "$base.now"
    if [ -n "$text" ]; then
        printf '%s' "$text" > "$base.stdin"; in="$base.stdin"
    fi
    run_lc "$in" "$@" > "$base.raw-out" 2> "$base.raw-err"
    printf '%s\n' "$?" > "$base.rc"
    normalize < "$base.raw-out" > "$base.stdout"
    normalize < "$base.raw-err" > "$base.stderr"
    rm -f "$base.raw-out" "$base.raw-err"
}
lc() { record "" "$@"; }
lc_in() { local t="$1"; shift; record "$t" "$@"; }

# shellcheck source=cases.sh
. "$cases"
declare -F seed >/dev/null || die "$cases defines no seed"

mkdir -p "$out" || die "cannot create $out"
out="$(realpath "$out")"
rm -rf "${out:?}"/*
: > "$out/$MARK"
mkdir -p "$out/_seed"

# The testdb: either the caller's stopped store, copied, or a fresh one seeded here.
rm -rf "$work/source"; mkdir -p "$work/source"
if [ -n "$source_dir" ]; then
    cp -a "$source_dir/." "$work/source/data" || die "cannot copy $source_dir"
    rm -rf "$out/_seed"
else
    mkdir -p "$work/source/data"
    start_server "$work/source/data"
    printf 'CREATE DATABASE IF NOT EXISTS spira_lifecycle;\n' > "$work/create.sql"
    root_sql < "$work/create.sql" >/dev/null 2>&1 || die "cannot create spira_lifecycle"
    # A fresh database takes its whole shape from schema.sql; the migrations are for old ones.
    run_lc "" admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$work/schema.err" 2>&1 \
        || die "schema.sql: $(normalize < "$work/schema.err")"
    STEP_DIR="$out/_seed" STEP_N=0
    seed
    printf "USE spira_lifecycle; CALL DOLT_ADD('-A'); CALL DOLT_COMMIT('-m', 'lc-parity seed', '--author', 'lc-parity <lc-parity@localhost>', '--date', '2026-01-01T00:00:00Z');\n" > "$work/commit.sql"
    root_sql < "$work/commit.sql" >"$work/commit.err" 2>&1 || die "cannot commit the seed: $(normalize < "$work/commit.err")"
    stop_server
    if [ -n "$save_seed" ]; then
        mkdir -p "$save_seed" && cp -a "$work/source/data/." "$save_seed" || die "cannot save the seed to $save_seed"
    fi
fi
for f in "$out/_seed"/*.rc; do
    [ -e "$f" ] || continue
    [ "$(cat "$f")" = 0 ] || die "seed step $(basename "$f" .rc) exited $(cat "$f"): $(cat "${f%.rc}.stderr")"
done

for c in $(declare -F | awk '{print $3}' | grep '^case_' | sort); do
    name="${c#case_}"; name="${name//_/-}"
    rm -rf "$work/case"; mkdir -p "$work/case"
    cp -a "$work/source/data" "$work/case/data" || die "cannot copy the testdb for $name"
    start_server "$work/case/data"
    STEP_DIR="$out/$name" STEP_N=0 NOW="$NOW_EPOCH"
    mkdir -p "$STEP_DIR"
    "$c"
    stop_server
done
echo "lc-parity: $(find "$out" -name '*.rc' | wc -l) steps in $(find "$out" -mindepth 1 -maxdepth 1 -type d | wc -l) directories at $out"
