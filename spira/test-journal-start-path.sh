#!/usr/bin/env bash
#
# test-journal-start-path.sh — the reads spira-runtime makes to open its engine answer fast
# on a journal of production size, through the real spira_lc grant set.
#
#   ./test-journal-start-path.sh
#
# WHAT THIS PROVES:
#   - on ~325k journal rows, the open-effects statement reaches runtime_event only by key, so
#     its cost does not grow with history (its time is printed with the start-path verbs');
#   - open-effects names exactly the requests no outcome closed, whether the tracking row was
#     written by the migration backfill or by the append path, and never one an older writer
#     left unclosed.
#
# host-reason: starts its own disposable `dolt sql-server`, as test-lifecycle-migrate.sh does.
#
# tier: T2
# covers: lifecycle/migrations/* lifecycle/schema.sql lifecycle/grants.sql spira-lc/src/journal.rs runtime/src/socket.rs spira-runtime/src/lib.rs
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

. "$HERE/conf.sh"
export PATH="$PATH:$(dirname "$DOLT_BIN")"
unset SPIRA_LC_SOCKET

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((3309 + 1000 + (RANDOM % 300)))
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

root_sql() { timeout 120 "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls "$@"; } # batch-job: seeding 325k rows is a bulk load

# spira-lc is the tree under test's own build, found by name on the suite's PATH (sp-gypjk).
command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
# SPIRA_LC_PASSWORD_FILE EXPLICITLY, EMPTY: it is a registered key, resolved from SPIRA_TOML
# alone now — left undeclared it falls to the complete fixture's own (nonexistent) path, and
# every spira-lc call refuses before reaching the dolt server. Empty matches root's actual
# password here; switched to the real one below once this suite moves to spira_lc.
LC_CRED="$TMP/lc.credential"; : > "$LC_CRED"
tl_config SPIRA_LC_PASSWORD_FILE="$LC_CRED"

spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
[ $? -eq 0 ] || { cat "$TMP/schema.log" >&2; bail "the lifecycle schema did not apply"; }
cat "$TMP/schema.log" >&2

PASS="test-pass-$$"
sed "s/@SPIRA_LC_PASSWORD@/$PASS/" "$REPO/lifecycle/grants.sql" > "$TMP/grants_filled.sql"
root_sql sql < "$TMP/grants_filled.sql" >"$TMP/grants.log" 2>&1
[ $? -eq 0 ] || { cat "$TMP/grants.log" >&2; bail "the lifecycle grants did not apply"; }
cat "$TMP/grants.log" >&2

# From here on, every `spira-lc` call runs AS spira_lc — the real production grant set (design
# §3.6: "only spira_lc writes spira_lifecycle"), never root. A cascade that only works with
# superuser privileges would be a false green.
export SPIRA_LC_USER=spira_lc
export SPIRA_LC_PASSWORD="$PASS"
printf '%s' "$PASS" > "$LC_CRED"
tl_config SPIRA_LC_PASSWORD_FILE="$LC_CRED"


# Production shape: ~25% of events are requests, nearly all closed; five stay open.
python3 - > "$TMP/seed.sql" <<'PY'
import random
random.seed(7)
streams = ["task.cockpit-tier-5s", "watch.round-duty", "runtime.heartbeat", "units", "task.sweep.archive"]
rows = []
for i in range(100000):
    s = random.choice(streams)
    rows.append((f"req-{i}", s, "effect.requested", None))
    rows.append((f"out-{i}", s, random.choice(["effect.succeeded"] * 8 + ["effect.failed", "effect.timed_out"]), f"req-{i}"))
    rows.append((f"st-{i}", s, "state.changed", None))
    if i % 4 == 0:
        rows.append((f"hb-{i}", "runtime.heartbeat", "heartbeat", None))
for i in range(5):
    rows.append((f"open-{i}", "task.x", "effect.requested", None))
rows.append(("halt-1", "scheduler.halt", "scheduler.halted", None))
for k in range(0, len(rows), 2000):
    vals = ",".join("('%s','%s','%s','%s',1,%s,'c')" % (a, b, c, "x" * 200, "'%s'" % d if d else "NULL") for a, b, c, d in rows[k:k + 2000])
    print("INSERT INTO runtime_event (event_id,stream,kind_tag,kind_body,at,cause,correlation) VALUES " + vals + ";")
PY
root_sql --use-db spira_lifecycle sql < "$TMP/seed.sql" >/dev/null 2>&1
wantrc "the production-size journal seeds" 0 $?
is "...to ~310k rows" "yes" "$(root_sql --use-db spira_lifecycle sql -q "SELECT IF(COUNT(*) > 300000, 'yes', 'no') AS v FROM runtime_event" -r json | python3 -c 'import json,sys; print(json.load(sys.stdin)["rows"][0]["v"])')"

for f in "$REPO"/lifecycle/migrations/*runtime-effect*.sql; do
    [ -f "$f" ] || continue
    sed -n '/^INSERT/,$p' "$f" | root_sql --use-db spira_lifecycle sql >/dev/null 2>&1
    wantrc "the backfill in $(basename "$f") applies" 0 $?
done

# The statement open-effects runs, as shipped; its plan must reach runtime_event only by key.
SQL="$(grep -o "SELECT r.seq, r.event_id.*ORDER BY r.seq" "$REPO/spira-lc/src/journal.rs" | head -1)"
[ -n "$SQL" ] || bail "the open-effects statement was not found in journal.rs"
PLAN="$(root_sql --use-db spira_lifecycle sql -q "EXPLAIN PLAN $SQL")"
unkeyed="$(printf '%s\n' "$PLAN" | grep -A3 "IndexedTableAccess(runtime_event)" | grep -c "filters: \[{\[NULL, ∞)")"
wantrc "POSITIVE CONTROL: the matcher sees a full-range access" 0 "$(printf 'IndexedTableAccess(runtime_event)\n index: [runtime_event.event_id]\n filters: [{[NULL, ∞)}]\n' | grep -A3 "IndexedTableAccess(runtime_event)" | grep -q "filters: \[{\[NULL, ∞)"; echo $?)"
is "open-effects reads no part of runtime_event by range" "0" "$unkeyed"
want "...it finds the candidates by the open index" "index: [runtime_effect.resolved,runtime_effect.seq]" "$PLAN"

for step in "cursor" "stream scheduler.halt 1000000000 1000" "open-effects" "end" "due"; do
    t0=$(date +%s%N)
    # shellcheck disable=SC2086
    OUT="$(timeout 5 spira-lc journal $step 2>&1)"; RC=$?
    echo "# journal $step: rc=$RC $(( ($(date +%s%N) - t0) / 1000000 )) ms" >&2
    [ "$RC" = 0 ] || bad "journal $step failed: rc=$RC ${OUT:0:200}"
done
is "open-effects names the five requests nothing closed" "open-0 open-1 open-2 open-3 open-4" "$(timeout 5 spira-lc journal open-effects | python3 -c 'import json,sys; print(" ".join(sorted(e[1]["id"] for e in json.load(sys.stdin))))')"

ev() {   # ev <id> <tag> <cause|null>
    local cause="null"; [ "$3" = null ] || cause="\"$3\""
    printf '{"id":"%s","stream":"task.y","kind":{"tag":"%s","body":""},"at":2,"cause":%s,"correlation":"c"}' "$1" "$2" "$cause"
}
ids() { spira-lc journal open-effects | python3 -c 'import json,sys; print(" ".join(sorted(e[1]["id"] for e in json.load(sys.stdin))))'; }

spira-lc journal append "$(ev new-1 effect.requested null)" >/dev/null
is "an appended request is open at once" "new-1 open-0 open-1 open-2 open-3 open-4" "$(ids)"
spira-lc journal append "$(ev new-1-done effect.succeeded new-1)" >/dev/null
is "its outcome closes it" "open-0 open-1 open-2 open-3 open-4" "$(ids)"
spira-lc journal append "$(ev open-2-done effect.failed open-2)" >/dev/null
is "an outcome closes a backfilled request" "open-0 open-1 open-3 open-4" "$(ids)"

root_sql --use-db spira_lifecycle sql -q "UPDATE runtime_effect SET resolved = 0 WHERE event_id = 'open-2'" >/dev/null
is "a row an older writer left unclosed is not reported open" "open-0 open-1 open-3 open-4" "$(ids)"

spira-lc journal group '{"writes":[{"append":'"$(ev grp-1 effect.requested null)"'},{"append":'"$(ev grp-1-done effect.timed_out grp-1)"'},{"append":'"$(ev grp-2 effect.requested null)"'}]}' >/dev/null
is "a group opens and closes in order" "grp-2 open-0 open-1 open-3 open-4" "$(ids)"

tl_summary
