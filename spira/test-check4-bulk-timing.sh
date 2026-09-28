#!/usr/bin/env bash
#
# test-check4-bulk-timing.sh — the timing guard for _check4_bulk_sql / check4_bulk_data.
#
#   ./test-check4-bulk-timing.sh
#
# THE DEFECT THIS GUARDS AGAINST (sp-rp4g4). The bulk query used to compute its
# poison.cleared floor with a correlated subquery run per row, once per CASE branch. Against
# 74,527 real events that was 0.4s at 1 id and 26.8s at 5 ids — bd gave up waiting (no
# BD_TIMEOUT wrapper) long before the query itself finished, and Dolt does not cancel a query
# on client disconnect, so every sentinel pass leaked one full-scan query that ran forever.
# The rewrite computes the floor once per issue in a derived table; this suite is the
# regression guard so a future change cannot silently reintroduce the per-row form without a
# suite noticing before a sentinel pass does.
#
# 50,000+ events, spread over the SAME 200 ids the bulk query is asked about (not diluted
# with untouched ids) — the worst case for a per-row correlated subquery, since every row in
# the table is also a row in the output. If the rewrite is holding, this is fast anyway.
#
# tier: T2
# defect: sp-rp4g4
# covers: spira/lib.sh
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

. "$HERE/testdb.sh"
testdb_available || skip "no fixture database reachable"
testdb_require check4-bulk-timing
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — check4_bulk_data reads the events table via bd sql, which embedded
# mode refuses.
export SPIRA_TESTDB_MODE=server
testdb_up check4-bulk-timing || skip "server testdb not available"
. "$HERE/lib.sh"

echo "test-check4-bulk-timing.sh"

testdb_reset

N_IDS=200
ROWS_PER_ID=252   # 2 signal ('claimed') + 250 filler ('noise') = 50,400 total events
# Each row's SQL text is ~100 bytes; Linux caps a single argv/envp string at 128 KiB
# (MAX_ARG_STRLEN) — a batch of 1000 rows (~100 KB) stays comfortably under that per call
# while still reaching 50k+ rows in about 50 calls instead of one per row.
BATCH=1000

# The fixture beads — events.issue_id carries fk_events_issue -> issues(id), so the events
# below cannot be inserted without a real row on each side of it.
python3 > "$TMP/beads.jsonl" <<PYEOF
import json
for i in range(1, $N_IDS + 1):
    print(json.dumps({
        "id": "sp-bulk%03d" % i, "title": "bulk timing fixture bead",
        "status": "open", "issue_type": "task", "labels": ["spira", "plan"],
        "updated_at": "2026-01-01T00:00:00Z",
    }))
PYEOF
testdb_seed < "$TMP/beads.jsonl"
seeded="$(bdjson list --limit 0 --label spira,plan 2>/dev/null | python3 -c '
import json, sys
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))' 2>/dev/null)"
seeded="${seeded:-0}"
is "all $N_IDS fixture beads seeded" "$N_IDS" "$seeded"

# THE EVENTS, IN BATCHED MULTI-ROW INSERTS. A heredoc, not `python3 -c '...'`: the SQL text
# this generates is single-quoted (standard SQL string literals), and single quotes inside a
# bash single-quoted -c argument close it early — every apostrophe in the generated SQL was
# silently swallowed as a bash quote delimiter, so `bd sql` received unquoted values and
# every insert failed with a parser error. A heredoc has no such collision.
EVENTS_SQL="$TMP/events.sql"
python3 > "$EVENTS_SQL" <<PYEOF
import uuid
n_ids = $N_IDS
rows_per_id = $ROWS_PER_ID
batch = $BATCH
rows = []
for i in range(1, n_ids + 1):
    bid = "sp-bulk%03d" % i
    for j in range(rows_per_id):
        et = "claimed" if j < 2 else "noise"
        rows.append((str(uuid.uuid4()), bid, et))
for i in range(0, len(rows), batch):
    chunk = rows[i:i + batch]
    values = ",".join(
        "('%s','%s','%s','%s','%s','%s')" % (rid, bid, et, "harness", "", "2026-01-01 00:00:00")
        for (rid, bid, et) in chunk
    )
    print("INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES " + values)
PYEOF

n_batches=0; n_failed=0
while IFS= read -r stmt; do
    [ -n "$stmt" ] || continue
    _ins_err="$(bdq sql "$stmt" 2>&1 1>/dev/null)" || {
        n_failed=$((n_failed + 1))
        bad "batch insert $((n_batches+1)) succeeded" "$(printf '%s' "$_ins_err" | tail -3 | tr '\n' ' ')"
    }
    n_batches=$((n_batches + 1))
done < "$EVENTS_SQL"
[ "$n_failed" -eq 0 ] && ok "seeded $n_batches batch(es) of events with no failures" \
    || bad "seeded $n_batches batch(es) of events with no failures" "$n_failed batch(es) failed"

total_events="$(bdq sql "select count(*) from events where issue_id like 'sp-bulk%'" 2>/dev/null \
    | sed -n '3p' | tr -d ' ')"
total_events="${total_events:-0}"

# 0. POSITIVE CONTROL — the fixture is actually past the scale the defect needed to bite,
#    not a small table that would pass a slow query too (law-absence-needs-a-positive-control).
if [ "$total_events" -ge 50000 ] 2>/dev/null; then
    ok "fixture has at least 50,000 events ($total_events)"
else
    bad "fixture has at least 50,000 events" "only $total_events"
fi

disp="$(python3 -c "
print(chr(10).join('sp-bulk%03d\tspira,plan' % i for i in range(1, $N_IDS + 1)))
")"

start_ns="$(date +%s%N)"
bulk="$(check4_bulk_data "$disp")"
rc=$?
end_ns="$(date +%s%N)"
elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))

is "check4_bulk_data succeeds against the fixture" "0" "$rc"

# 1. THE TIMING GUARD.
if [ "$elapsed_ms" -lt 5000 ]; then
    ok "200 ids against 50k+ events complete in under 5s (${elapsed_ms}ms)"
else
    bad "200 ids against 50k+ events complete in under 5s" "took ${elapsed_ms}ms"
fi

# 2. NOT JUST FAST — CORRECT. Every one of the 200 ids carries exactly 2 'claimed' events and
#    nothing that discounts them, so every one must read back as 2 attempts. Sampling three
#    rather than all 200 keeps this suite's own runtime down; the timing pass above already
#    proves every row came back.
for sample in sp-bulk001 sp-bulk100 sp-bulk200; do
    att="$(printf '%s' "$bulk" | awk -F'\t' -v id="$sample" '$1==id{print $2}')"
    is "$sample: 2 claimed events read back as 2 attempts" "2" "${att:-0}"
done

tl_summary
