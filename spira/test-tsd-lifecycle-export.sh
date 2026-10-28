#!/usr/bin/env bash
#
# test-tsd-lifecycle-export.sh — tsd-lifecycle-export (sp-qz2yj), the one writer of
# the bead-stage family, held in the metrics store (design reconciler-time-series-2026-09-27 §2/§2a).
#
# WHAT THIS SUITE CHECKS, against real dependencies (a real bd fixture store's own `events`
# table, and a real throwaway `dolt sql-server` for spira_lifecycle — never a hand-modelled
# stub of either):
#   1. legacy: every bd audit event maps to exactly one bead-stage row, lifecycle state
#      names on from_state/to_state.
#   2. re-running legacy exports nothing new (the high-water-mark checkpoint).
#   3. backfill: the row count matches bd's own count of events since the backfill date.
#   4. lifecycle: a spira_lifecycle.event fixture yields the same schema, including a row
#      with applied=false (a real refusal, not synthesized).
#   5. no harness transition site writes to run/tsd/'s bead-stage family directly — only
#      this exporter does.
#
# The unit-level fold logic (every legacy event type's effect, the checkpoint's boundary-tie
# dedupe, the lifecycle passthrough) is covered by tsd-lifecycle-export's own `cargo test`
# (T0, no I/O) — this suite is the I/O wiring: real bd, real dolt, real tsd-write.
#
# defect: sp-qz2yj
# tier: T2
# covers: tsd-lifecycle-export/* tsd/src/* spira-otel/src/* spira/conf.sh spira/testdb.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH"
export PATH="$PATH:$(dirname "$DOLT_BIN")"

REPO="$(cd "$HERE/.." && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# ── 0. the two binaries this suite drives: the tree's own build, by name on PATH (sp-gypjk) ──
command -v tsd-lifecycle-export >/dev/null 2>&1 && command -v tsd-write >/dev/null 2>&1 \
    && command -v tsd-read >/dev/null 2>&1 && command -v tsd-fake-store >/dev/null 2>&1 \
    || bail "tsd-lifecycle-export, tsd-write, tsd-read and tsd-fake-store must all be on PATH"
EXPORT_BIN=tsd-lifecycle-export

# One fake metrics store per scenario: the exporter's rows go to the store, never to a file.
STORE_PIDS=""
start_store() {  # start_store <name> — sets STORE_ADDR
    tsd-fake-store >"$T/$1.addr" 2>/dev/null &
    STORE_PIDS="$STORE_PIDS $!"
    local n=0
    while [ ! -s "$T/$1.addr" ] && [ "$n" -lt 100 ]; do sleep 0.05; n=$((n + 1)); done
    STORE_ADDR="$(head -1 "$T/$1.addr")"
    [ -n "$STORE_ADDR" ] || bail "tsd-fake-store did not start"
}
stage_rows() {  # stage_rows <run-dir> <store> <out-file> — the family's rows from the store, oldest first
    SPIRA_RUN="$1" SPIRA_METRICS_ENDPOINT="$2" tsd-read rows bead-stage 999999 >"$3" 2>"$3.err"
}

jpy() {  # jpy <file> <python-expr-on-"rows"> — rows is a list of parsed JSON lines
    python3 -c '
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
print(eval(sys.argv[2]))
' "$1" "$2"
}

# ============================================================================================
printf '\n%s\n' "1-2. legacy: bd events -> bead-stage rows, and a re-run exports nothing new"
# ============================================================================================
. "$HERE/testdb.sh"
testdb_available || skip "no fixture database reachable"
testdb_require tsd-lifecycle-export
# testdb-mode: server — the fixture is seeded with `bdq sql` INSERT/DELETE on events, and `bd sql` is refused in embedded mode.
export SPIRA_TESTDB_MODE=server
testdb_up tsd-lifecycle-export || skip "server testdb not available"
trap 'testdb_drop; rm -rf "$T"' EXIT INT TERM
. "$HERE/lib.sh"
testdb_reset

testdb_seed <<'JSONL'
{"id":"sp-tle-a","title":"claimed then closed","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-16T00:00:00Z"}
{"id":"sp-tle-b","title":"claimed only","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
# testdb_seed's own bd create writes its own audit rows (create, label add x2 per bead) —
# real bd behaviour, not noise to filter around at query time. Cleared for these two ids so
# the fixture below controls its own event stream exactly, the same precedent
# test-attempts-sql.sh's per-bead counting suite already relies on for its own seeded ids.
bdq sql "DELETE FROM events WHERE issue_id IN ('sp-tle-a','sp-tle-b')" >/dev/null 2>&1

seedt() {   # seedt <id> <event_type> <new_value> <created_at>
    local id="$1" et="$2" nv="$3" ts="$4" uuid
    uuid="$(python3 -c 'import uuid; print(uuid.uuid4())')"
    bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', '$et', 'dwarf-1', '$nv', '$ts')" >/dev/null 2>&1
}
seedt sp-tle-a claimed ''    '2026-09-16T00:00:01Z'
seedt sp-tle-a closed  ''    '2026-09-16T00:00:02Z'
seedt sp-tle-b claimed ''    '2026-09-16T00:00:03Z'

RUN1="$T/run1"; mkdir -p "$RUN1"
start_store store1; STORE1="$STORE_ADDR"
trap 'kill $STORE_PIDS 2>/dev/null; testdb_drop; rm -rf "$T"' EXIT INT TERM
run_export() {   # run_export <mode> <run-dir> <store> [--since <ts>]
    local mode="$1" run="$2" store="$3"; shift 3
    tl_config SPIRA_RUN="$run" SPIRA_BD="$SPIRA_BD"
    SPIRA_DB="$SPIRA_DB" SPIRA_METRICS_ENDPOINT="$store" \
        "$EXPORT_BIN" "$mode" "$@"
}
run_export legacy "$RUN1" "$STORE1" --since '2026-09-16T00:00:00Z' >"$T/export1.out" 2>&1
wantrc "legacy export exits 0" 0 $?
cat "$T/export1.out" >&2

FAM1="$T/fam1.jsonl"
stage_rows "$RUN1" "$STORE1" "$FAM1" && ok "bead-stage rows stored" || bad "MUST-FAIL CHECK: no bead-stage rows in the store: $(cat "$FAM1.err")"
[ -e "$RUN1/tsd/bead-stage.jsonl" ] && bad "the exporter wrote a bead-stage file beside the store" || ok "no bead-stage file is written"
is "exactly one row per bd event (3 seeded)" "3" "$(jpy "$FAM1" 'len(rows)')"
is "row 1: source is legacy"      "legacy"  "$(jpy "$FAM1" 'rows[0]["source"]')"
is "row 1: machine is bead"       "bead"    "$(jpy "$FAM1" 'rows[0]["machine"]')"
is "row 1: key is the bead id"    "sp-tle-a" "$(jpy "$FAM1" 'rows[0]["key"]')"
is "row 1 (claimed): from_state"  "READY"   "$(jpy "$FAM1" 'rows[0]["from_state"]')"
is "row 1 (claimed): to_state"    "WORKING" "$(jpy "$FAM1" 'rows[0]["to_state"]')"
is "row 1: applied is true"       "True"    "$(jpy "$FAM1" 'rows[0]["applied"]')"
is "row 3 (sp-tle-b's own claim): from_state is READY, not sp-tle-a's WORKING" \
   "READY" "$(jpy "$FAM1" 'rows[2]["from_state"]')"
seqs="$(jpy "$FAM1" '[r["seq"] for r in rows]')"
is "seq is assigned in order" "[1, 2, 3]" "$seqs"

# ── re-run: nothing new ──────────────────────────────────────────────────────────────────
run_export legacy "$RUN1" "$STORE1" --since '2026-09-16T00:00:00Z' >"$T/export1b.out" 2>&1
wantrc "second legacy export exits 0" 0 $?
stage_rows "$RUN1" "$STORE1" "$FAM1"
is "re-running exports nothing new (still 3 rows)" "3" "$(jpy "$FAM1" 'len(rows)')"

# ── a new event after the checkpoint IS picked up ────────────────────────────────────────
seedt sp-tle-b closed '' '2026-09-16T00:00:04Z'
run_export legacy "$RUN1" "$STORE1" --since '2026-09-16T00:00:00Z' >"$T/export1c.out" 2>&1
stage_rows "$RUN1" "$STORE1" "$FAM1"
is "a genuinely new event is exported on the next run" "4" "$(jpy "$FAM1" 'len(rows)')"
is "the new row's seq continues from the checkpoint" "4" "$(jpy "$FAM1" 'rows[3]["seq"]')"

# ============================================================================================
printf '\n%s\n' "3. backfill: row count matches bd's own event count since the backfill date"
# ============================================================================================
RUN2="$T/run2"; mkdir -p "$RUN2"
start_store store2; STORE2="$STORE_ADDR"
run_export backfill "$RUN2" "$STORE2" --since '2026-09-16T00:00:00Z' >"$T/export2.out" 2>&1
wantrc "backfill export exits 0" 0 $?
cat "$T/export2.out" >&2
FAM2="$T/fam2.jsonl"
stage_rows "$RUN2" "$STORE2" "$FAM2"
n_events="$(bdq sql --json "SELECT COUNT(*) AS n FROM events WHERE created_at >= '2026-09-16T00:00:00Z'" 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["n"])')"
n_rows="$(jpy "$FAM2" 'len(rows)')"
is "backfill row count matches bd events since the backfill date" "$n_events" "$n_rows"
is "backfill rows are tagged with their own source" "backfill" "$(jpy "$FAM2" 'rows[0]["source"]')"

# ============================================================================================
printf '\n%s\n' "4. lifecycle source: a spira_lifecycle.event fixture, applied=false included"
# ============================================================================================
LTMP="$T/lc"; mkdir -p "$LTMP/data"
LPORT=$((13307 + (RANDOM % 500)))
cat > "$LTMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $LPORT
  max_connections: 50
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$LTMP/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
"$DOLT_BIN" sql-server --config "$LTMP/server.yaml" > "$LTMP/server.log" 2>&1 & # batch-job: fixture dolt call against the suite's private store
LC_SERVER_PID=$!
trap 'kill "$LC_SERVER_PID" $STORE_PIDS >/dev/null 2>&1; testdb_drop; rm -rf "$T"' EXIT INT TERM

up=0
for _ in $(seq 1 50); do
    "$DOLT_BIN" --data-dir "$LTMP" --host 127.0.0.1 --port "$LPORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1 && { up=1; break; } # batch-job: fixture dolt call against the suite's private store
    sleep 0.2
done
[ "$up" = 1 ] || bail "lifecycle dolt sql-server never came up: $(cat "$LTMP/server.log")"
root_lc_sql() { "$DOLT_BIN" --data-dir "$LTMP" --host 127.0.0.1 --port "$LPORT" -u root -p "" --no-tls "$@"; } # batch-job: fixture dolt call against the suite's private store

root_lc_sql sql < "$REPO/lifecycle/schema.sql" >"$LTMP/schema.log" 2>&1
wantrc "spira_lifecycle schema applies cleanly" 0 $?

root_lc_sql --use-db spira_lifecycle sql -q \
    "INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, refusal, evidence, actor, at) VALUES ('bead','sp-tle-c','Claim','READY','READY','WORKING',1,NULL,'{}','dwarf-2',1758500000)" \
    >/dev/null 2>&1
root_lc_sql --use-db spira_lifecycle sql -q \
    "INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, refusal, evidence, actor, at) VALUES ('bead','sp-tle-c','GateRed','SUBMITTED','SUBMITTED','SUBMITTED',0,'ExpectMismatch','{\"reason\":\"flaky\"}','verdict',1758500010)" \
    >/dev/null 2>&1

RUN3="$T/run3"; mkdir -p "$RUN3"
start_store store3; STORE3="$STORE_ADDR"
# SPIRA_LC_PASSWORD_FILE is registered and resolves ambiently via cfg() to the complete
# fixture's own nonexistent path, which would be preferred over the explicit
# SPIRA_LC_PASSWORD="" below (sfail round 3, pattern 7 — same cause as test-canary.sh).
tl_config SPIRA_RUN="$RUN3" SPIRA_LC_PASSWORD_FILE=""
SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$LPORT" SPIRA_LC_DB=spira_lifecycle SPIRA_LC_DATA_DIR="$LTMP" \
    SPIRA_LC_USER=root SPIRA_LC_PASSWORD="" SPIRA_METRICS_ENDPOINT="$STORE3" \
    "$EXPORT_BIN" lifecycle >"$T/export3.out" 2>&1
wantrc "lifecycle export exits 0" 0 $?
cat "$T/export3.out" >&2

FAM3="$T/fam3.jsonl"
stage_rows "$RUN3" "$STORE3" "$FAM3" && ok "bead-stage rows stored from the lifecycle source" \
                || bad "MUST-FAIL CHECK: no bead-stage rows from the lifecycle source: $(cat "$FAM3.err")"
is "both event rows are exported" "2" "$(jpy "$FAM3" 'len(rows)')"
is "row 1: source is lifecycle"     "lifecycle" "$(jpy "$FAM3" 'rows[0]["source"]')"
is "row 1: applied is true"         "True"      "$(jpy "$FAM3" 'rows[0]["applied"]')"
is "row 2: applied is false (a real refusal, not synthesized)" \
   "False" "$(jpy "$FAM3" 'rows[1]["applied"]')"
is "row 2: refusal is carried"      "ExpectMismatch" "$(jpy "$FAM3" 'rows[1]["refusal"]')"
is "row 2: reason pulled from evidence" "flaky" "$(jpy "$FAM3" 'rows[1]["reason"]')"

# re-run: nothing new
tl_config SPIRA_RUN="$RUN3"
SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$LPORT" SPIRA_LC_DB=spira_lifecycle SPIRA_LC_DATA_DIR="$LTMP" \
    SPIRA_LC_USER=root SPIRA_LC_PASSWORD="" SPIRA_METRICS_ENDPOINT="$STORE3" \
    "$EXPORT_BIN" lifecycle >"$T/export3b.out" 2>&1
stage_rows "$RUN3" "$STORE3" "$FAM3"
is "re-running the lifecycle source exports nothing new" "2" "$(jpy "$FAM3" 'len(rows)')"

kill "$LC_SERVER_PID" >/dev/null 2>&1
trap 'kill $STORE_PIDS 2>/dev/null; testdb_drop; rm -rf "$T"' EXIT INT TERM

# ============================================================================================
printf '\n%s\n' "5. no harness transition site writes to run/tsd/'s bead-stage family directly"
# ============================================================================================
offenders="$(grep -rl -- "--family bead-stage\|family=bead-stage\|family bead-stage" \
    "$REPO"/spira/*.sh "$REPO"/lifecycle*/src/*.rs "$REPO"/spira-lc/src/*.rs \
    "$REPO"/batcher/src/*.rs "$REPO"/batcher-cut/src/*.rs "$REPO"/queue-watch/src/*.rs 2>/dev/null \
    | grep -v '/tsd-lifecycle-export/' | grep -v '/test-tsd-lifecycle-export\.sh$' || true)"
is "no transition site writes bead-stage directly — only tsd-lifecycle-export does" "" "$offenders"

tl_summary
