#!/usr/bin/env bash
#
# epic-rank.sh — print ready and submitted beads grouped by epic, in the same epic-first
# rank order aeon.sh's own claim uses (sp-ns46j): the parent epic's priority, then a started
# epic before an unstarted one at equal priority, then the bead's own priority within the
# epic. The round cutter: the Concierge (and later the batcher) reads this to cut one
# epic's beads into a round at a time instead of interleaving unrelated epics.
#
#   epic-rank.sh [--label <csv>] [--exclude-label <csv>]
#
# READY, PLUS SUBMITTED — never just ready. A round that only picked up unclaimed work
# would cut a round missing the epic's own beads already finished and waiting on the
# landing pass, which is exactly the work a round needs to close out an epic.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

LABEL="" EXCLUDE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --label) LABEL="$2"; shift 2 ;;
        --exclude-label) EXCLUDE="$2"; shift 2 ;;
        *) die "usage: epic-rank.sh [--label <csv>] [--exclude-label <csv>]" ;;
    esac
done

ready_args=(ready --limit 0 --exclude-type epic,event -u)
[ -n "$LABEL" ] && ready_args+=(--label "$LABEL")
[ -n "$EXCLUDE" ] && ready_args+=(--exclude-label "$EXCLUDE")
ready_json="$(bdjson "${ready_args[@]}")"
[ -n "$ready_json" ] || ready_json="[]"

submitted_args=(list --status open --limit 0 --exclude-type epic,event
                --label "${SPIRA_SUBMITTED_LABEL:-spira-submitted}")
[ -n "$LABEL" ] && submitted_args+=(--label "$LABEL")
[ -n "$EXCLUDE" ] && submitted_args+=(--exclude-label "$EXCLUDE")
submitted_json="$(bdjson "${submitted_args[@]}")"
[ -n "$submitted_json" ] || submitted_json="[]"

# READY AND SUBMITTED CAN NEVER OVERLAP (a claimed bead is not ready), but a bead is kept
# from whichever query named it first rather than trusted to appear in only one.
all_json="$(python3 -c '
import json, sys
a = json.loads(sys.argv[1]); b = json.loads(sys.argv[2])
a = a if isinstance(a, list) else [a]
b = b if isinstance(b, list) else [b]
seen = {r["id"] for r in a}
print(json.dumps(a + [r for r in b if r["id"] not in seen]))
' "$ready_json" "$submitted_json")"

epic_lookup="$(epic_parent_lookup "$all_json")"

epic_rank_rows "$all_json" "$epic_lookup" "" | python3 -c '
import sys

order = []
groups = {}
meta = {}
for line in sys.stdin:
    line = line.rstrip("\n")
    if not line:
        continue
    eprio, estarted, bprio, resumable, age, bid, epic_id = line.split("\t")
    if epic_id not in groups:
        groups[epic_id] = []
        order.append(epic_id)
        meta[epic_id] = (eprio, estarted)
    groups[epic_id].append(bid)

for epic_id in order:
    eprio, estarted = meta[epic_id]
    tag = "started" if estarted == "0" else "unstarted"
    print("== %s (P%s, %s) ==" % (epic_id, eprio, tag))
    for bid in groups[epic_id]:
        print("  %s" % bid)
'
