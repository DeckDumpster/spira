#!/usr/bin/env bash
# test-census-burst.sh — census ranks a burst of one class as one occurrence, not one per bead
#
# One operator action touching many beads in the same minute used to rank as that many
# failures; and one teardown reported under two names (unjudged-killed by the component
# that killed the session, unjudged-slain by the aeon's own cleanup) ranked twice.
#
# Positive control: on the unfixed ranker the first assertion reads "8 ... (8 detections" /
# a leading 8 for the folded class, and the spaced class below still ranks 5 — the spaced
# class is what proves the window is not simply collapsing everything.
#
# tier: T2
# covers: census/src/* spira/census/* spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-census-burst
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — census_events_run_sql uses bd sql, which embedded mode refuses
export SPIRA_TESTDB_MODE=server
testdb_up census-burst || {
    printf 'SKIP test-census-burst: server testdb not available\n' >&2
    exit 77
}
# shellcheck disable=SC1090
. "$HERE/lib.sh"

_insert_event_at() {   # <bead_id> <event_type> <cause> <utc_ts>
    local uuid
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))')" || return 1
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql \
        "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$1', '$2', 'harness', '$3', '$4')" \
        >/dev/null 2>&1
}

echo "test-census-burst.sh"
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-b1","title":"b1","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-29T00:00:00Z"}
{"id":"sp-b2","title":"b2","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-29T00:00:00Z"}
{"id":"sp-b3","title":"b3","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-29T00:00:00Z"}
{"id":"sp-b4","title":"b4","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-29T00:00:00Z"}
{"id":"sp-b5","title":"b5","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-29T00:00:00Z"}
{"id":"sp-b6","title":"b6","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-29T00:00:00Z"}
JSONL

# Fleet slay: six beads 7s apart, each reported killed then slain ~60s later; then a
# second sweep hours later touching two of them.
for n in 1 2 3 4 5 6; do
    _insert_event_at "sp-b$n" requeued unjudged-slain "2026-09-29 03:58:$((40 + n))"
    _insert_event_at "sp-b$n" requeued unjudged-killed "2026-09-29 03:58:$((10 + n))"
done
_insert_event_at sp-b1 requeued unjudged-slain "2026-09-29 09:18:11"
_insert_event_at sp-b2 requeued unjudged-slain "2026-09-29 09:18:16"
# Genuinely independent: five beads, ten minutes apart.
for n in 1 2 3 4 5; do
    _insert_event_at "sp-b$n" recurred spaced "2026-09-29 12:$((n * 10)):00"
done

out="$(census_events_run_sql 2>/dev/null | python3 "$HERE/census/count.py")"
want "killed/slain folded to one class, two sweeps = 2 occurrences, 6 beads, 14 detections" \
    "2 14 sp-requeue-unjudged-killed 6" "$out"
nowant "no separate unjudged-slain class" "sp-requeue-unjudged-slain" "$out"
want "independent events ten minutes apart still rank one each" "5 5 sp-recur-spaced" "$out"

fold="$(_census_class_fold_map)"
want "fold map aliases slain onto killed" "sp-requeue-unjudged-slain sp-requeue-unjudged-killed" "$fold"

since="$(census_events_run_sql 1790649952 2>/dev/null | python3 "$HERE/census/count.py")"
want "windowed query collapses the same way" "2 14 sp-requeue-unjudged-killed 6" "$since"

tl_summary
