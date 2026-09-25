#!/usr/bin/env bash
#
# test-strand-partition.sh — starved-queue escalations cite only beads from their own
#   partition; a saturated fleet suppresses escalation; slot allocation appears in detail.
#
#   ./test-strand-partition.sh
#
# WHY THIS EXISTS. sp-15u9f: a stranded-queue escalation titled [spira,incident] cited a
# bead belonging to spira,qa-sweep. Both parties were right about their own evidence, which
# is the most expensive kind of wrong alarm — contradictory evidence trains the operator to
# disbelieve true ones. The fix: the detail cites beads from the same partition as the
# title (both come from classify_one's filtered `ready` set), and a saturated fleet is
# queue ordering rather than starvation.
#
# FIVE CASES (law-absence-needs-a-positive-control):
#
#   0. POSITIVE CONTROL — free fleet slot, starved partition → classifier DOES produce
#      "starved". Proves the check can fire before we trust its silence in cases 1 and 2.
#
#   1. SATURATED FLEET — every slot held by another partition → "fleet-saturated" info
#      row, NOT "starved". The harness is doing the right thing; do not page the operator.
#
#   2. PARTITION ISOLATION — two partitions starved simultaneously: the escalation for
#      partition A names only partition A's beads, never partition B's. A single-partition
#      test could pass by accident; two partitions prove isolation.
#
#   3. SLOT DETAIL — the starved row names "0 of N slot(s)" and the total-live count, so
#      the operator can confirm the fleet state in one glance without opening the panel.
#
#   4. FREE-SLOT POSITIVE PATH — a fleet with live aeons but spare capacity still
#      escalates, confirming the saturated-fleet suppression is narrowly scoped.
#
#   5. FULL STACK — strand.sh check --from end-to-end, same partition-isolation property
#      as case 2 but through the shell layer and mail.sh, not just the classifier.
#
#   6-8. D7 MERGE (from test-reclaim-needs-ryan.sh, deleted): the ghost half of the same
#      classifier. 6 is the positive control (plain dead worker IS ghost); 7 and 8 are the
#      two exemptions (the ask label directly, and check2_protect_waiting's skip label) that
#      stop CHECK 2b from re-reclaiming a bead CHECK 2 already excluded (sp-2k5a, sp-qsa1).
#
# PRE-FIX FAILURE (run against unfixed strand-classify.py):
#
#   FAIL  saturated fleet: fleet-saturated IS emitted: wanted [fleet-saturated] in [starved\t...]
#   FAIL  saturated fleet: starved NOT emitted: did not want [starved] in [starved\t...]
#   FAIL  slot detail: "0 of 3 slot(s)" in starved row: wanted [0 of 3 slot(s)] in [...]
#   FAIL  partition A isolation: sp-pa1 cited in A escalation: wanted [sp-pa1] in []
#   FAIL  partition B isolation: sp-pb1 NOT cited in A escalation: did not want [sp-pb1] in [sp-pb1]
#
# defect: sp-15u9f
# covers: spira/strand-classify.py spira/strand.sh
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run"

# Minimal bead fixture: one ready bead per partition.
BEADS_A='[{"id":"sp-pa1","title":"plan bead","status":"open","labels":["spira","plan"]}]'
BEADS_B='[{"id":"sp-pb1","title":"incident bead","status":"open","labels":["spira","incident"]}]'
READY_A="$BEADS_A"
READY_B="$BEADS_B"

classify() {
    local beads_json="$1" ready_json="$2" live="${3:-0}" total_live="${4:-0}" max_aeons="${5:-0}"
    local tmpb tmpr
    tmpb="$(mktemp "$TMP/beads-XXXXX.json")"
    tmpr="$(mktemp "$TMP/ready-XXXXX.json")"
    printf '%s' "$beads_json" > "$tmpb"
    printf '%s' "$ready_json" > "$tmpr"
    BEADS_FILE="$tmpb" \
    READY_FILE="$tmpr" \
    HOLDERS="" LIVE="$live" GHOST_GRACE=300 \
    TOTAL_LIVE="$total_live" MAX_AEONS="$max_aeons" \
    SPIRA_ASK_LABEL=needs-operator \
    CAPACITY_PAUSED=0 \
        python3 "$HERE/strand-classify.py"
    rm -f "$tmpb" "$tmpr"
}

# classify_ghost <beads-json> — the ghost half of the classifier, in isolation: an expired
# lease (PAST, well outside GHOST_GRACE=0), no live holder, no ready set at all. D7: merged
# in from test-reclaim-needs-ryan.sh, which drove exactly this shape.
PAST="2020-01-01T00:00:00Z"
classify_ghost() {
    printf '%s' "$1" > "$TMP/ghost-beads.json"
    printf '[]' > "$TMP/ghost-ready.json"
    BEADS_FILE="$TMP/ghost-beads.json" \
    READY_FILE="$TMP/ghost-ready.json" \
    HOLDERS="" LIVE=1 GHOST_GRACE=0 \
    SPIRA_ASK_LABEL=needs-operator \
    SPIRA_RECLAIM_SKIP_LABEL=spira-waiting-operator \
        python3 "$HERE/strand-classify.py"
}

echo "test-strand-partition.sh"

# ======================================================================================
echo
echo "case 0 — positive control: free slot, LIVE=0 → classifier DOES produce starved:"
# ======================================================================================
# Without this half, a classifier that never fires looks correct when we test silence
# in cases 1 and 2 (law-absence-needs-a-positive-control).
out="$(classify "$BEADS_A" "$READY_A" 0 1 3)"
want "positive control: starved IS raised"         "starved"        "$out"
nowant "positive control: fleet-saturated NOT in output" "fleet-saturated" "$out"

# ======================================================================================
echo
echo "case 1 — saturated fleet: TOTAL_LIVE=MAX_AEONS → fleet-saturated info, not starved:"
# ======================================================================================
# Every slot is held by another partition. Escalating would page the operator about
# correct scheduling. Emit fleet-saturated (info, not escalate) so cmd_check ignores it
# while strand.sh report still shows it.
out="$(classify "$BEADS_A" "$READY_A" 0 3 3)"
want   "saturated fleet: fleet-saturated IS emitted" "fleet-saturated" "$out"
nowant "saturated fleet: starved NOT emitted"        "starved"         "$out"

# ======================================================================================
echo
echo "case 2 — partition isolation: two partitions starved; each cites only its own beads:"
# ======================================================================================
# The detail in each starved row comes from the `ready` set that was filtered to that
# partition's labels — so sp-pa1 appears only in partition A's row, sp-pb1 only in B's.
# This is tested here at the classifier level; the shell layer (classify_one) filters
# by label before calling us, so the filtered sets never mix.
out_a="$(classify "$BEADS_A" "$READY_A" 0 0 3)"
out_b="$(classify "$BEADS_B" "$READY_B" 0 0 3)"

want   "partition A: sp-pa1 cited"     "sp-pa1" "$out_a"
nowant "partition A: sp-pb1 NOT cited" "sp-pb1" "$out_a"
want   "partition B: sp-pb1 cited"     "sp-pb1" "$out_b"
nowant "partition B: sp-pa1 NOT cited" "sp-pa1" "$out_b"

# ======================================================================================
echo
echo "case 3 — slot detail: starved row carries '0 of N slot(s)' and total-live count:"
# ======================================================================================
# The detail must name the slot allocation so the operator can verify fleet state in one
# glance without opening the panel (acceptance criterion: body states slot allocation).
out="$(classify "$BEADS_A" "$READY_A" 0 1 3)"
want "slot detail: '0 of 3 aeon slot(s)' in starved row" "0 of 3 aeon slot(s)" "$out"
want "slot detail: total-live count in starved row"  "1 live across fleet" "$out"

# ======================================================================================
echo
echo "case 4 — free-slot positive path: live aeons exist but fleet has spare capacity → escalates:"
# ======================================================================================
# TOTAL_LIVE=1, MAX_AEONS=3 means 2 spare slots exist; this partition has LIVE=0.
# The harness has capacity to summon; starvation is real and must be escalated.
out="$(classify "$BEADS_A" "$READY_A" 0 1 3)"
want   "spare capacity: starved IS raised"            "starved"        "$out"
nowant "spare capacity: fleet-saturated NOT raised"   "fleet-saturated" "$out"

# ======================================================================================
echo
echo "case 5 — full stack: escalation for partition A carries its bead, not partition B's:"
# ======================================================================================
# Uses strand.sh check --from to exercise the full escalation path. SPIRA_LABELS pins the
# partition attribution; the detail (from strand-classify.py) must match only A's beads.
printf 'starved\t-\tescalate\t1 bead(s) ready and no live aeon; 0 of 3 aeon slot(s) are serving this partition (1 live across fleet): sp-pa1\tcheck sentinel\n' \
    > "$TMP/fixture_a.tsv"
printf 'starved\t-\tescalate\t1 bead(s) ready and no live aeon; 0 of 3 aeon slot(s) are serving this partition (1 live across fleet): sp-pb1\tcheck sentinel\n' \
    > "$TMP/fixture_b.tsv"
echo "sentinel ok" > "$TMP/run/sentinel.log"

ARGS_A="$TMP/mail-args-a"; ARGS_B="$TMP/mail-args-b"
mkdir -p "$TMP/home-a" "$TMP/home-b"
# Unquoted heredoc so $ARGS_A/$ARGS_B expand now; \${1:-} and \$@ escape as script literals.
cat > "$TMP/home-a/mail.sh" <<STUB
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
printf '%s\n' "\$@" >> "$ARGS_A"
cat >> "$ARGS_A"
STUB
cat > "$TMP/home-b/mail.sh" <<STUB
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
printf '%s\n' "\$@" >> "$ARGS_B"
cat >> "$ARGS_B"
STUB
chmod +x "$TMP/home-a/mail.sh" "$TMP/home-b/mail.sh"

env \
    SPIRA_RUN="$TMP/run" \
    SPIRA_STRAND_GRACE=0 \
    SPIRA_LABELS=spira,plan \
    SPIRA_HOME="$TMP/home-a" \
    bash "$HERE/strand.sh" check --from "$TMP/fixture_a.tsv" >/dev/null 2>&1

env \
    SPIRA_RUN="$TMP/run" \
    SPIRA_STRAND_GRACE=0 \
    SPIRA_LABELS=spira,incident \
    SPIRA_HOME="$TMP/home-b" \
    bash "$HERE/strand.sh" check --from "$TMP/fixture_b.tsv" >/dev/null 2>&1

args_a="$(cat "$ARGS_A" 2>/dev/null || true)"
args_b="$(cat "$ARGS_B" 2>/dev/null || true)"
want   "full stack partition A: sp-pa1 in evidence" "sp-pa1" "$args_a"
nowant "full stack partition A: sp-pb1 NOT in evidence" "sp-pb1" "$args_a"
want   "full stack partition A: title names plan partition" "spira,plan" "$args_a"
want   "full stack partition B: sp-pb1 in evidence" "sp-pb1" "$args_b"
nowant "full stack partition B: sp-pa1 NOT in evidence" "sp-pa1" "$args_b"
want   "full stack partition B: title names incident partition" "spira,incident" "$args_b"

# ======================================================================================
echo
echo "case 6 — D7 merge, positive control: plain dead worker IS classified ghost:"
# ======================================================================================
# A bead whose aeon died with no special protection. ghost must fire — without this, an
# implementation that never raises ghost is indistinguishable from a correct one.
out="$(classify_ghost '[
  {"id":"sp-dead","title":"dead worker","status":"in_progress",
   "labels":["spira","plan"],"assignee":"aeon-dead",
   "lease_expires_at":"'"$PAST"'"}
]')"
want  "plain dead worker: ghost raised"  "ghost"    "$out"
want  "plain dead worker: bead named"    "sp-dead"  "$out"

echo
echo "case 7 — D7 merge: bead carrying the ask label directly is NOT ghost:"
# ======================================================================================
# sp-2k5a: an aeon filed a needs-ryan decision bead and exited. The work bead carries
# needs-operator, its lease is stale, no live aeon holds it. Reclaiming it re-summons a
# session that immediately re-derives the same diagnosis and exits (the respawn loop).
out="$(classify_ghost '[
  {"id":"sp-ask","title":"escalated bead","status":"in_progress",
   "labels":["needs-operator","spira","plan"],"assignee":"aeon-x",
   "lease_expires_at":"'"$PAST"'"}
]')"
nowant "ask-labeled bead: ghost NOT raised"  "ghost"   "$out"
nowant "ask-labeled bead: bead NOT named"    "sp-ask"  "$out"

echo
echo "case 8 — D7 merge: bead carrying check2_protect_waiting's skip label is NOT ghost:"
# ======================================================================================
# check2_protect_waiting labels a work bead with spira-waiting-operator when its only open
# dep carries the ask label, so the sentinel's --exclude-label skips it in CHECK 2. The
# ghost check (CHECK 2b) must honour the same exclusion, or it reclaims what CHECK 2
# explicitly protected.
out="$(classify_ghost '[
  {"id":"sp-protected","title":"waiting for ryan","status":"in_progress",
   "labels":["spira-waiting-operator","spira","plan"],"assignee":"aeon-y",
   "lease_expires_at":"'"$PAST"'"}
]')"
nowant "skip-labeled bead: ghost NOT raised"   "ghost"        "$out"
nowant "skip-labeled bead: bead NOT named"     "sp-protected" "$out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
