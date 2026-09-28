#!/usr/bin/env bash
#
# test-epic-claim-order.sh — the epic-first claim rank (sp-ns46j, per Ryan 2026-09-28):
# rank ready beads by their parent epic's priority, then a started epic before an unstarted
# one at equal priority, then the bead's own priority, then resumable-before-fresh, then
# oldest. Covers epic_parent_lookup/epic_rank_rows (lib.sh) directly and aeon.sh's wiring
# of them into the actual claim.
#
# THE DEFECT THIS FIXES. `bd ready --claim` orders by bead priority alone, so a bead ejected
# back to ready from the epic being delivered competes on its own P-number with every
# unrelated P0 — narrow the fleet to a few aeons and the epic stalls behind work that has
# nothing to do with it. T3 below reproduces exactly that shape: a started P0 epic's
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
# defect: sp-ns46j
# covers: spira/lib.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-epic-claim-order
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT
trap 'testdb_drop; rm -rf "$TMP"; exit 143' TERM
testdb_up epicclaimorder || { echo "test-epic-claim-order.sh: could not build a fixture database"; exit 1; }

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such.conf"
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
    local id="$1" parent="$2" prio="$3" deps="" labels="\"plan\""
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
echo "T2: equal epic priority — a STARTED epic outranks an unstarted one, even against a"
echo "    better bead priority"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 1)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-p2 sp-e1 2)
$(epic sp-e2 1)
$(bead sp-e2-p0 sp-e2 0)
JSONL
order="$(ranked_ids)"
is "E1 (started) at P1: its P2 child outranks E2's (unstarted) P0 child" \
    "sp-e1-p2
sp-e2-p0" "$order"

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
ready_json="$(SPIRA_BD="$BIN_BD/bd-count" bdjson ready --limit 0 --exclude-type epic,event -u --label plan)"
: > "$CALL_LOG"
SPIRA_BD="$BIN_BD/bd-count" epic_parent_lookup "$ready_json" >/dev/null
prio_calls="$(grep -c '^-C .*list --id ' "$CALL_LOG" 2>/dev/null || echo 0)"
is "exactly one 'bd list --id ...' call resolves every epic's own priority (5 beads, 2 epics)" \
    "1" "$prio_calls"
children_calls="$(grep -c '^-C .*children ' "$CALL_LOG" 2>/dev/null || echo 0)"
is "the 'started' check is one 'bd children' call per DISTINCT epic (2), not per bead (5)" \
    "2" "$children_calls"

# ==========================================================================================
echo
echo "T6: aeon.sh's own claim, wired end to end, claims the epic-first candidate — not"
echo "    whatever 'bd ready --claim' would have taken on bead priority alone"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 0)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-rework sp-e1 3)
$(bead sp-unrelated-p0 "" 0)
JSONL
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

AEON_HOME="$TMP/aeonhome"; mkdir -p "$AEON_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$HERE/suite-covers.sh" "$AEON_HOME/"
cp -r "$HERE/actors" "$AEON_HOME/" 2>/dev/null || true
AEON_RUN="$TMP/aeonrun"; mkdir -p "$AEON_RUN"
AEON_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$AEON_REPO_MAP"
cat > "$AEON_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}plan"
FAYTH_EXCLUDE_LABELS="spira-poison"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$AEON_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'claimed\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id - claimed"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

( SPIRA_HOME="$AEON_HOME" SPIRA_RUN="$AEON_RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$AEON_REPO_MAP" \
  SPIRA_CONF="$TMP/no-such2.conf" \
  "$AEON_HOME/aeon.sh" builder > "$TMP/aeon-out" 2>&1 )

# A task bead's close is converted to open + spira-submitted at teardown (sp-qsona): only
# the landing pass closes a work bead directly, so "claimed and finished" reads as
# open+submitted, not in_progress or closed.
e1_status="$(bd -C "$SPIRA_DB" show sp-e1-rework --json 2>/dev/null | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
print(d.get("status"))' 2>/dev/null)"
e1_labels="$(bd -C "$SPIRA_DB" show sp-e1-rework --json 2>/dev/null | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
print(",".join(d.get("labels") or []))' 2>/dev/null)"
unrelated_status="$(bd -C "$SPIRA_DB" show sp-unrelated-p0 --json 2>/dev/null | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
print(d.get("status"))' 2>/dev/null)"
is "aeon.sh claimed and finished the started epic's reworked child, not the unrelated P0 head" \
    "open" "$e1_status"
case ",$e1_labels," in
    *",spira-submitted,"*) ok "sp-e1-rework: carrying the submitted label (its work was done)" ;;
    *) bad "sp-e1-rework: carrying the submitted label (its work was done)" "labels=[$e1_labels]" ;;
esac
is "the unrelated P0 bead was left alone, still ready and unclaimed" "open" "$unrelated_status"
want "the log names the epic-first rank as the reason" "epic-first rank" "$(cat "$TMP/aeon-out")"

tl_summary
