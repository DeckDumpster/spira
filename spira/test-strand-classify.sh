#!/usr/bin/env bash
#
# test-strand-classify.sh — strand-classify.py: one table, one shared `starved` positive
#   control, one row per deliberate-state exemption and per boundary case.
#
#   ./test-strand-classify.sh
#
# WHY ONE FILE. Six suites (test-strand-capacity/pool-paused/partition/deferred/
# ghost-teardown/truncated.sh) each built their own BEADS_FILE/READY_FILE fixture and their
# own copy of "positive control: starved IS raised" — four byte-identical or near-identical
# controls proving the same fact. Merged here as duplicate cluster D7. Every case below
# still carries the defect id and the WHY that motivated it; only the scaffolding merges.
#
# capacity-paused (sp-9rscu): a healthy account decline is not a strand.
# pool-paused (sp-2es5u): SPIRA_MAX_AEONS=0 is a deliberate hold, not a fault
#   (law-a-deliberate-state-is-not-a-fault).
# fleet-saturated / slot detail / partition isolation (sp-15u9f): an escalation must cite
#   only its own partition's beads — the most expensive kind of wrong alarm is one where
#   both parties are individually right about their own evidence.
# deferred-unescalated (sp-hg8q): a bead deferred behind a live blocking edge is the DAG
#   doing its job, not a strand.
# ghost / mid-teardown (sp-nc74): a bead whose aeon is still tearing down (pidfile present)
#   must not be reclaimed out from under it even past GHOST_GRACE. ghost / ask+skip label
#   (sp-2k5a): a bead legitimately waiting on the operator, directly or via
#   check2_protect_waiting's skip label, is exempt the same way.
# pass-truncated (sp-ow8n): a sentinel pass that ran out of budget before reaching a
#   partition looks identical to "nothing ready" unless the classifier is told so.
#
# GAP G6 (strand.sh:218 classify_one, untested until now): every case above feeds the
# classifier directly through BEADS_FILE/READY_FILE, bypassing the shell layer that
# actually calls bd. The partition-isolation case proves the CLASSIFIER never mixes rows
# from two partitions it is handed separately — it says nothing about whether classify_one
# ASKS bd for the right partition in the first place. classify_one does no client-side
# label re-filtering of its own; it trusts `bd list --label "$labels"` entirely. So the one
# thing worth proving at the shell layer is that the label reaching bd is the right one —
# a stub bd that records its argv and returns a fixed (deliberately mixed-partition) JSON
# regardless of what it is asked, so the assertion is on the REQUEST, not a filtered
# response the stub could get right by accident.
#
# tier: T1
# covers: spira/strand-classify.py spira/strand.sh spira/aeon.sh
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run"

# One ready bead, no live aeons — the fixture every deliberate-state case suppresses.
BEADS='[{"id":"sp-example","title":"test bead","status":"open","labels":["spira","plan"]}]'
READY="$BEADS"

classify() {   # classify <beads-json> <ready-json> [VAR=val ...]
    printf '%s' "$1" > "$TMP/beads.json"
    printf '%s' "${2:-[]}" > "$TMP/ready.json"
    shift 2
    env \
        BEADS_FILE="$TMP/beads.json" \
        READY_FILE="$TMP/ready.json" \
        HOLDERS="" LIVE=0 GHOST_GRACE=300 \
        SPIRA_ASK_LABEL=needs-operator \
        CAPACITY_PAUSED=0 CAPACITY_DETAIL="" POOL_PAUSED=0 PASS_TRUNCATED=0 \
        TOTAL_LIVE=0 MAX_AEONS=0 \
        "$@" \
        python3 "$HERE/strand-classify.py"
}

echo "test-strand-classify.sh"

# ======================================================================================
echo
echo "positive control — ready work, no live aeon, nothing deliberate → starved:"
# ======================================================================================
# Every case below proves SILENCE under some deliberate state. Without this control first,
# a classifier that never fires would make every one of them pass by accident
# (law-absence-needs-a-positive-control).
out="$(classify "$BEADS" "$READY")"
want "positive control: starved IS raised" "starved" "$out"

# ======================================================================================
echo
echo "capacity-paused (sp-9rscu): the account's own window declining is not a strand:"
# ======================================================================================
out="$(classify "$BEADS" "$READY" CAPACITY_PAUSED=1 "CAPACITY_DETAIL=out for another 1800s")"
want   "capacity paused: capacity-paused IS emitted" "capacity-paused"          "$out"
nowant "capacity paused: starved NOT emitted"        "starved"                 "$out"
want   "capacity paused: CAPACITY_DETAIL in row"     "out for another 1800s"   "$out"

# ======================================================================================
echo
echo "pool-paused (sp-2es5u): SPIRA_MAX_AEONS=0 is a deliberate hold, not a fault:"
# ======================================================================================
out="$(classify "$BEADS" "$READY" POOL_PAUSED=1)"
want   "pool paused: pool-paused IS emitted"           "pool-paused" "$out"
want   "pool paused: pool-paused has info disposition" "info"        "$out"
nowant "pool paused: starved NOT emitted"              "starved"    "$out"

# ======================================================================================
echo
echo "fleet-saturated (sp-15u9f): every slot held by another partition is scheduling, not starvation:"
# ======================================================================================
out="$(classify "$BEADS" "$READY" LIVE=0 TOTAL_LIVE=3 MAX_AEONS=3)"
want   "saturated fleet: fleet-saturated IS emitted" "fleet-saturated" "$out"
nowant "saturated fleet: starved NOT emitted"        "starved"         "$out"

# ======================================================================================
echo
echo "slot detail (sp-15u9f): a starved row names its own slot allocation and total-live:"
# ======================================================================================
# TOTAL_LIVE=1, MAX_AEONS=3: two spare slots exist and this partition has none of them —
# starvation is real (also proves the saturated suppression above is narrowly scoped).
out="$(classify "$BEADS" "$READY" LIVE=0 TOTAL_LIVE=1 MAX_AEONS=3)"
want "slot detail: starved IS raised"                    "starved"              "$out"
want "slot detail: '0 of 3 aeon slot(s)' in starved row" "0 of 3 aeon slot(s)"  "$out"
want "slot detail: total-live count in starved row"      "1 live across fleet" "$out"

# ======================================================================================
echo
echo "partition isolation (sp-15u9f): two partitions starved; each cites only its own beads:"
# ======================================================================================
# The most expensive kind of wrong alarm: both parties are individually right about their
# own evidence. A single-partition test could pass by accident; two partitions prove
# isolation. (The classifier-level guarantee only — gap G6 below proves the shell layer
# that decides WHICH beads reach the classifier in the first place.)
BEADS_A='[{"id":"sp-pa1","title":"plan bead","status":"open","labels":["spira","plan"]}]'
BEADS_B='[{"id":"sp-pb1","title":"incident bead","status":"open","labels":["spira","incident"]}]'
out_a="$(classify "$BEADS_A" "$BEADS_A" LIVE=0 TOTAL_LIVE=0 MAX_AEONS=3)"
out_b="$(classify "$BEADS_B" "$BEADS_B" LIVE=0 TOTAL_LIVE=0 MAX_AEONS=3)"
want   "partition A: sp-pa1 cited"     "sp-pa1" "$out_a"
nowant "partition A: sp-pb1 NOT cited" "sp-pb1" "$out_a"
want   "partition B: sp-pb1 cited"     "sp-pb1" "$out_b"
nowant "partition B: sp-pa1 NOT cited" "sp-pa1" "$out_b"

# ======================================================================================
echo
echo "deferred-unescalated (sp-hg8q): a live blocking edge is the DAG doing its job:"
# ======================================================================================
# case 1 — live blocker (in_progress dep): NOT escalated.
out="$(classify '[
  {"id":"sp-blocker","title":"live work","status":"in_progress","labels":["spira","plan"]},
  {"id":"sp-blocked","title":"waiting downstream","status":"deferred","labels":["spira","plan"],
   "dependencies":[{"issue_id":"sp-blocked","depends_on_id":"sp-blocker","type":"blocks"}]}
]' '[]')"
nowant "live blocker: deferred-unescalated NOT raised" "deferred-unescalated" "$out"

# case 2 — dead blocker (all deps closed): IS escalated.
out="$(classify '[
  {"id":"sp-blocker","title":"done work","status":"closed","labels":["spira","plan"]},
  {"id":"sp-blocked","title":"waiting downstream","status":"deferred","labels":["spira","plan"],
   "dependencies":[{"issue_id":"sp-blocked","depends_on_id":"sp-blocker","type":"blocks"}]}
]' '[]')"
want "dead blocker: deferred-unescalated IS raised" "deferred-unescalated" "$out"
want "dead blocker: sp-blocked named"               "sp-blocked"           "$out"

# case 3 — no blockers at all: the original defect this check exists for. IS escalated.
out="$(classify '[
  {"id":"sp-naked","title":"naked deferred","status":"deferred","labels":["spira","plan"]}
]' '[]')"
want "no blockers: deferred-unescalated IS raised" "deferred-unescalated" "$out"
want "no blockers: sp-naked named"                 "sp-naked"             "$out"

# case 4 — mixed blockers (one live + one closed): one live blocker is sufficient.
out="$(classify '[
  {"id":"sp-live1","title":"still open","status":"open","labels":["spira","plan"]},
  {"id":"sp-done1","title":"already closed","status":"closed","labels":["spira","plan"]},
  {"id":"sp-mixed","title":"waiting on two","status":"deferred","labels":["spira","plan"],
   "dependencies":[
     {"issue_id":"sp-mixed","depends_on_id":"sp-live1","type":"blocks"},
     {"issue_id":"sp-mixed","depends_on_id":"sp-done1","type":"blocks"}
   ]}
]' '[]')"
nowant "mixed blockers: deferred-unescalated NOT raised" "deferred-unescalated" "$out"

# case 5 — ask-labelled: already in the operator's queue; double-reporting adds noise.
out="$(classify '[
  {"id":"sp-asked","title":"operator decision","status":"deferred","labels":["spira","plan","needs-operator"]}
]' '[]')"
nowant "ask-labelled: deferred-unescalated NOT raised" "deferred-unescalated" "$out"

# ======================================================================================
echo
echo "ghost (sp-nc74, sp-2k5a): mid-teardown, ask-label and skip-label are exempt:"
# ======================================================================================
# A lease_expires_at far enough in the past to exceed any GHOST_GRACE value.
EXPIRED="2000-01-01T00:00:00Z"

classify_ghost() {   # classify_ghost <beads-json> [holders]
    printf '%s' "$1" > "$TMP/ghost-beads.json"
    printf '[]' > "$TMP/ghost-ready.json"
    BEADS_FILE="$TMP/ghost-beads.json" \
    READY_FILE="$TMP/ghost-ready.json" \
    HOLDERS="${2:-}" LIVE=1 GHOST_GRACE=300 \
    SPIRA_ASK_LABEL=needs-operator \
    SPIRA_RECLAIM_SKIP_LABEL=spira-waiting-operator \
        python3 "$HERE/strand-classify.py"
}

GHOST_BEAD='[{"id":"sp-victim","title":"mid-teardown bead","status":"in_progress","labels":["spira","plan"],"lease_expires_at":"'"$EXPIRED"'"}]'

# case 1 — positive control: pidfile absent (holder=0) with expired lease IS ghost. Without
# this half, a classifier that never fires looks correct in case 2.
out="$(classify_ghost "$GHOST_BEAD" "sp-victim	0")"
want "no-pidfile: ghost IS raised" "ghost"     "$out"
want "no-pidfile: sp-victim named" "sp-victim" "$out"

# case 2 — mid-teardown: pidfile present (holder=1) with expired lease is NOT ghost. The
# aeon is still running (tearing down after a long fixture_drop). This is the invariant
# the fix in aeon.sh enforces by keeping the pidfile alive until after the bead operations
# complete (gap G7, aeon teardown ordering itself, is aeon-execution's scope, not this
# area's — cross-referenced only).
out="$(classify_ghost "$GHOST_BEAD" "sp-victim	1")"
nowant "with-pidfile: ghost NOT raised" "ghost" "$out"

# case 3 — ask-labelled (sp-2k5a): an escalated bead is legitimately waiting for the
# operator's decision. Reclaiming it re-summons an aeon that immediately re-derives the
# same diagnosis and exits — the loop that prompted the fix. No holder entry at all: the
# exemption must hold even though the /proc check alone would call this one a ghost.
out="$(classify_ghost '[
  {"id":"sp-ask","title":"escalated bead","status":"in_progress",
   "labels":["needs-operator","spira","plan"],"assignee":"aeon-x",
   "lease_expires_at":"'"$EXPIRED"'"}
]')"
nowant "ask-labelled: ghost NOT raised" "ghost"  "$out"
nowant "ask-labelled: bead NOT named"   "sp-ask" "$out"

# case 4 — skip-labelled: check2_protect_waiting labels a work bead with the skip label
# when its only open dep carries the ask label, so CHECK 2's --exclude-label skips it. The
# ghost check must honour the same exclusion, or it reclaims what CHECK 2 explicitly
# protected.
out="$(classify_ghost '[
  {"id":"sp-protected","title":"waiting for the operator","status":"in_progress",
   "labels":["spira-waiting-operator","spira","plan"],"assignee":"aeon-y",
   "lease_expires_at":"'"$EXPIRED"'"}
]')"
nowant "skip-labelled: ghost NOT raised" "ghost"        "$out"
nowant "skip-labelled: bead NOT named"   "sp-protected" "$out"

# ======================================================================================
echo
echo "pass-truncated (sp-ow8n): a sentinel pass out of budget looks like nothing ready:"
# ======================================================================================
# The shared positive control above already proves PASS_TRUNCATED=0 fires starved; only
# the suppression row is unique to this case. strand.sh's own detection of the "not
# evaluated" log line, and the pass-budget behaviour itself, stay in test-strand-truncated.sh
# — this file covers only the classifier's disposition once PASS_TRUNCATED is set.
out="$(classify "$BEADS" "$READY" PASS_TRUNCATED=1)"
want   "pass-truncated IS emitted" "pass-truncated" "$out"
nowant "starved IS NOT emitted"    "starved"         "$out"
want   "disposition is info"       "info"            "$out"

# ======================================================================================
echo
echo "gap G6 — classify_one threads its own partition's label to bd, not a mixed query:"
# ======================================================================================
# classify_one does no client-side re-filtering of its own; it trusts bd's --label entirely
# (strand.sh:218-231). Every case above bypasses that layer by feeding BEADS_FILE/READY_FILE
# directly. Here a stub bd IGNORES --label and always returns beads from two partitions —
# on purpose, so the assertion below is on the ARGV strand.sh sent, not on a response the
# stub could get right by accident. If a future change dropped or mis-threaded --label,
# this is the one place that would catch it.
ARGV_LOG="$TMP/g6-argv.log"; : > "$ARGV_LOG"
cat > "$TMP/mock-bd-g6" <<MOCKBD
#!/usr/bin/env bash
case "\${1:-}" in -C) shift 2 ;; esac
printf '%s\n' "\$*" >> "$ARGV_LOG"
case "\${1:-}" in
    list)  printf '[{"id":"sp-mixed-a","title":"a","status":"open","labels":["spira","plan"]},{"id":"sp-mixed-b","title":"b","status":"open","labels":["spira","incident"]}]\n' ;;
    ready) printf '[]\n' ;;
    *) exit 0 ;;
esac
MOCKBD
chmod +x "$TMP/mock-bd-g6"

mkdir -p "$TMP/g6-home/chamber"   # empty chamber: fayths_for_labels/spira_fayths find nothing

env \
    SPIRA_BD="$TMP/mock-bd-g6" \
    SPIRA_DB="/nonexistent-g6" \
    SPIRA_HOME="$TMP/g6-home" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_LABELS="spira,plan" \
    bash "$HERE/strand.sh" report >/dev/null 2>&1

argv="$(cat "$ARGV_LOG" 2>/dev/null || true)"
want "G6: bd list was asked for the requested partition's label" "--label spira,plan" "$argv"

tl_summary
