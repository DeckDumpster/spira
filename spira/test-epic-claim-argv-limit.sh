#!/usr/bin/env bash
#
# test-epic-claim-argv-limit.sh — T7 (sp-o4trx): a ready set past MAX_ARG_STRLEN (128 KiB)
# still ranks every bead. Split off test-epic-claim-order.sh for wall time; the shared
# fixture is testlib/epic-claim-fixture.sh.
#
# defect: sp-o4trx
# tier: T3
# covers: spira/lib.sh dwarf/src/* spira-claim/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90

ECL_SUITE=test-epic-claim-argv-limit
# shellcheck disable=SC1090
. "$HERE/testlib/epic-claim-fixture.sh"

echo "test-epic-claim-argv-limit.sh"

# ==========================================================================================
echo
echo "T7: a ready set past MAX_ARG_STRLEN (128 KiB) still ranks every bead (sp-o4trx)"
# ==========================================================================================
# THE CLIFF THIS REPRODUCES. epic_parent_lookup/epic_rank_rows used to pass the whole
# ready-set JSON to python3 -c as a single argv element. At 142 real ready beads that
# argument exceeded the kernel's per-argument limit (MAX_ARG_STRLEN, 128 KiB): the exec died
# "Argument list too long" (rc 126), both functions printed nothing, and every builder dwarf
# read the empty ranked list as "nothing ready to claim" for an hour (572 idle calls
# against 142 ready beads). Every other fixture in this suite uses a handful of beads, so
# none of them ever reached the threshold — this one must.
big_desc="$(python3 -c 'print("x" * 900)')"
N=100
{
    epic sp-ebig 0
    closed_child sp-ebig-done sp-ebig
    for i in $(seq 1 "$N"); do
        printf '{"id":"sp-big%03d","title":"t","status":"open","issue_type":"task","priority":%d,"labels":["plan","repo:fixture"],"updated_at":"2026-09-04T00:00:00Z","description":"%s","dependencies":[{"issue_id":"sp-big%03d","depends_on_id":"sp-ebig","type":"parent-child"}]}\n' \
            "$i" "$((i % 5))" "$big_desc" "$i"
    done
} > "$TMP/t7.jsonl"
seed < "$TMP/t7.jsonl"

t7_ready_json="$(bdjson ready --limit 0 --exclude-type epic,event -u --label plan)"
t7_size=${#t7_ready_json}
# 0. POSITIVE CONTROL. The case below is only meaningful if the payload is actually past
#    the limit; a fixture that shrank under it would pass against the broken code.
[ "$t7_size" -gt 131072 ] && ok "fixture ready-set JSON is past the 128 KiB limit ($t7_size bytes)" \
                          || bad "fixture ready-set JSON is only $t7_size bytes — not testing the cliff"

t7_order="$(ranked_ids)"
t7_count="$(printf '%s\n' "$t7_order" | grep -c .)"
is "every one of $N beads past the argv limit is ranked (none dropped by an E2BIG exec)" \
    "$N" "$t7_count"

t7_lookup_rc=0
epic_lookup="$(epic_parent_lookup "$t7_ready_json")" || t7_lookup_rc=$?
is "epic_parent_lookup succeeds (rc=0) against the oversized ready set" "0" "$t7_lookup_rc"

t7_rank_rc=0
epic_rank_rows "$t7_ready_json" "$epic_lookup" "" >/dev/null || t7_rank_rc=$?
is "epic_rank_rows succeeds (rc=0) against the oversized ready set" "0" "$t7_rank_rc"

tl_summary
