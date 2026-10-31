#!/usr/bin/env bash
#
# test-epic-claim-order.sh — the epic-first claim rank (sp-ns46j, per Ryan 2026-09-28):
# rank ready beads by min(epic priority, bead priority), then a started epic before an unstarted
# one at equal priority, then the bead's own priority, then resumable-before-fresh, then
# oldest. Covers epic_parent_lookup/epic_rank_rows (lib.sh) directly; dwarf's wiring of them
# into the actual claim is test-epic-claim-dwarf.sh (T6) and test-epic-claim-rank-failure.sh (T8);
# the oversized ready set is test-epic-claim-argv-limit.sh (T7). The fixture is
# testlib/epic-claim-fixture.sh.
#
# THE DEFECT THIS FIXES. `bd ready --claim` orders by bead priority alone, so a bead ejected
# back to ready from the epic being delivered competes on its own P-number with every
# unrelated P0 — narrow the fleet to a few dwarves and the epic stalls behind work that has
# nothing to do with it. test-epic-claim-dwarf.sh T6 reproduces exactly that shape: a started P0 epic's
# lower-priority child against a fresh, unrelated P0 bead.
#
# T1/T2 CANNOT FAIL FOR WANT OF A TARGET on today's code: epic_parent_lookup and
# epic_rank_rows do not exist before this bead, so they error out (command not found) rather
# than merely disagreeing with the expectation — the strongest form of "seen to fail".
#
# A REAL bd ON A THROWAWAY DATABASE (testdb.sh): the rank is a claim about what `bd list`,
# `bd children` and the labels/status/parent fields they return actually contain, which a
# stub would be a second, driftable opinion of (law-prefer-the-real-dependency).
#
# defect: sp-ns46j sp-o4trx
# tier: T3
# covers: spira/lib.sh dwarf/src/* spira-claim/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90

ECL_SUITE=test-epic-claim-order
# shellcheck disable=SC1090
. "$HERE/testlib/epic-claim-fixture.sh"

echo "test-epic-claim-order.sh"

# ==========================================================================================
echo
echo "T1: epic priority outranks bead priority"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 1)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-p2 sp-e1 2)
$(epic sp-e2 0)
$(bead sp-e2-p0 sp-e2 0)
JSONL
order="$(ranked_ids)"
is "E2's P0 child (its epic is P0) ranks before E1's P2 child (its epic is P1)" \
    "sp-e2-p0
sp-e1-p2" "$order"

# ==========================================================================================
echo
echo "T2: the first rank component is min(epic priority, bead priority); at equal effective"
echo "    priority a STARTED epic outranks an unstarted one"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 1)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-p2 sp-e1 2)
$(epic sp-e2 1)
$(bead sp-e2-p0 sp-e2 0)
JSONL
order="$(ranked_ids)"
is "a P0 child of an unstarted P1 epic outranks a P2 child of a started P1 epic" \
    "sp-e2-p0
sp-e1-p2" "$order"
seed <<JSONL
$(epic sp-e1 1)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-p1 sp-e1 1)
$(epic sp-e2 1)
$(bead sp-e2-p1 sp-e2 1)
JSONL
order="$(ranked_ids)"
is "equal effective priority keeps started-epic-first order" \
    "sp-e1-p1
sp-e2-p1" "$order"

# ==========================================================================================
echo
echo "T3: a reworked bead keeps its epic's rank — the starvation this bead fixes"
echo "    (Ryan, 2026-09-28: narrowing the fleet let a reworked bead lose to unrelated P0s)"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 0)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-rework sp-e1 3)
$(bead sp-unrelated-p0 "" 0)
JSONL
order="$(ranked_ids)"
is "E1's reworked P3 child (E1 is P0, started) outranks a fresh, unrelated P0 bead" \
    "sp-e1-rework
sp-unrelated-p0" "$order"
epic_id="$(rank_row sp-e1-rework | cut -f7)"
is "sp-e1-rework's rank still names E1 as its epic" "sp-e1" "$epic_id"

# ==========================================================================================
echo
echo "T4: a bead with no epic sorts by its own priority, and ranks as its own epic"
# ==========================================================================================
seed <<JSONL
$(bead sp-none-p0 "" 0)
$(bead sp-none-p2 "" 2)
JSONL
order="$(ranked_ids)"
is "no-epic beads: P0 before P2" "sp-none-p0
sp-none-p2" "$order"
epic_id="$(rank_row sp-none-p0 | cut -f7)"
is "a no-epic bead's epic_id is its own id" "sp-none-p0" "$epic_id"

# ==========================================================================================
echo
echo "T5: the parent lookup is ONE query, whatever the ready-set size — not one per bead"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 1)
$(closed_child sp-e1-done sp-e1)
$(epic sp-e2 2)
$(bead sp-e1-a sp-e1 1)
$(bead sp-e1-b sp-e1 2)
$(bead sp-e1-c sp-e1 3)
$(bead sp-e2-a sp-e2 1)
$(bead sp-e2-b sp-e2 2)
JSONL
REAL_BD="${SPIRA_BD:-$(command -v "${TESTDB_BD:-bd-embedded}")}"
BIN_BD="$TMP/bin"; mkdir -p "$BIN_BD"
CALL_LOG="$TMP/bd-calls.log"; : > "$CALL_LOG"
cat > "$BIN_BD/bd-count" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$CALL_LOG"
exec "$REAL_BD" "\$@"
STUB
chmod +x "$BIN_BD/bd-count"
# SPIRA_BD is a registered key (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare via
# tl_config, not the env prefixes below, which no process reads it from any more.
tl_config SPIRA_BD="$BIN_BD/bd-count"
ready_json="$(bdjson ready --limit 0 --exclude-type epic,event -u --label plan)"
: > "$CALL_LOG"
epic_parent_lookup "$ready_json" >/dev/null
prio_calls="$(grep -c '^-C .*list --id ' "$CALL_LOG" 2>/dev/null || echo 0)"
is "exactly one 'bd list --id ...' call resolves every epic's own priority (5 beads, 2 epics)" \
    "1" "$prio_calls"
children_calls="$(grep -c '^-C .*children ' "$CALL_LOG" 2>/dev/null || echo 0)"
is "the 'started' check is one 'bd children' call per DISTINCT epic (2), not per bead (5)" \
    "2" "$children_calls"

tl_summary
