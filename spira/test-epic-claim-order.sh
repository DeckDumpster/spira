#!/usr/bin/env bash
#
# test-epic-claim-order.sh — the epic-first claim rank (sp-ns46j, per Ryan 2026-09-28):
# rank ready beads by min(epic priority, bead priority), then a started epic before an unstarted
# one at equal priority, then the bead's own priority, then resumable-before-fresh, then
# oldest. Covers epic_parent_lookup/epic_rank_rows (lib.sh) directly; dwarf's wiring of them
# into the actual claim (T6/T8) is test-epic-claim-dwarf.sh, split off for wall time.
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
# T7 (sp-o4trx): a ready set past MAX_ARG_STRLEN (128 KiB) still ranks every bead.
#
# defect: sp-ns46j sp-o4trx
# tier: T3
# covers: spira/lib.sh dwarf/src/* spira-claim/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-epic-claim-order
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT
trap 'testdb_drop; rm -rf "$TMP"; exit 143' TERM
testdb_up epicclaimorder || { echo "test-epic-claim-order.sh: could not build a fixture database"; exit 1; }
# sp-mve9i: an epic is "started" by its children's lifecycle rows, never bd status. With no
# lifecycle store here, a stand-in spira-lc on PATH (spira-claim runs it by name) tells the
# fixture's bd story in lifecycle terms: closed → LANDED, in_progress → WORKING, open → READY.
lc_mirror_bd "$TMP/lc-mirror"
export PATH="$TMP/lc-mirror:$PATH" SPIRA_LC_BIN

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such.conf"
tl_config SPIRA_RUN="$SPIRA_RUN"
# round 2 fix: the complete fixture declares scope_label="spira" as its base value, so
# builder.rune's RUNE_LABELS (resolved against the real config, not this shell's unset
# $SPIRA_SCOPE_LABEL) would require a "spira" label bead()'s seeded beads never carry —
# nothing would ever be ready. Declare the empty scope this suite has always meant.
tl_config SPIRA_SCOPE_LABEL=""
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-epic-claim-order.sh"

seed() { testdb_reset; testdb_seed; }   # seed <<JSONL ... JSONL, after a clean slate

# epic <id> <priority>            -> one open epic bead
# closed_child <id> <parent>      -> one CLOSED child, makes <parent> "started"
# bead <id> <parent-or-""> <prio> -> one ready (open, plan-labelled, unparented-or-childed) task
epic()         { printf '{"id":"%s","title":"epic","status":"open","issue_type":"epic","priority":%s,"labels":[],"updated_at":"2026-09-04T00:00:00Z"}\n' "$1" "$2"; }
closed_child() { printf '{"id":"%s","title":"done child","status":"closed","issue_type":"task","priority":2,"labels":[],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"%s","type":"parent-child"}]}\n' "$1" "$1" "$2"; }
bead() {
    local id="$1" parent="$2" prio="$3" deps="" labels="\"plan\",\"repo:fixture\""
    [ -n "$parent" ] && deps=",\"dependencies\":[{\"issue_id\":\"$id\",\"depends_on_id\":\"$parent\",\"type\":\"parent-child\"}]"
    [ -n "${SPIRA_SCOPE_LABEL:-}" ] && labels="\"$SPIRA_SCOPE_LABEL\",$labels"
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","priority":%s,"labels":[%s],"updated_at":"2026-09-04T00:00:00Z"%s}\n' "$id" "$prio" "$labels" "$deps"
}

# ranked_ids -> the ready set's own ids (READY_ARGS shape, plan label), ranked best-first.
ranked_ids() {
    local ready_json epic_lookup
    ready_json="$(bdjson ready --limit 0 --exclude-type epic,event -u --label plan)"
    [ -n "$ready_json" ] || ready_json="[]"
    epic_lookup="$(epic_parent_lookup "$ready_json")"
    epic_rank_rows "$ready_json" "$epic_lookup" "" | cut -f6
}
# rank_row <id> -> that bead's own TSV row (eprio estarted bprio resumable age id epic_id)
rank_row() {
    local id="$1" ready_json epic_lookup
    ready_json="$(bdjson ready --limit 0 --exclude-type epic,event -u --label plan)"
    [ -n "$ready_json" ] || ready_json="[]"
    epic_lookup="$(epic_parent_lookup "$ready_json")"
    epic_rank_rows "$ready_json" "$epic_lookup" "" | awk -F'\t' -v id="$id" '$6 == id'
}

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
N=120
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
