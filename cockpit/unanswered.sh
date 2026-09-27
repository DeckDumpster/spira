#!/usr/bin/env bash
#
# unanswered.sh — threads where the operator spoke last and nobody answered.
#
#   unanswered.sh            list them, newest silence first
#   unanswered.sh --count    just the number, for the dashboard
#
# WHY (the operator, verbatim: "i've left comments on both of the remaining FYI items in the
# attention pane but still haven't seen your responses. i need positive acknowledgement in
# the pane.")
#
# Two of their comments sat unanswered for three hours and five hours. The watcher had been
# skipping insights outright, so nothing even told me they existed — but the deeper problem
# is that "did anyone answer them" was not a MEASURED state. It depended on me noticing,
# which is the same class of failure as a check that only reports success.
#
# The rule this encodes: a thread whose newest comment is their is an OPEN OBLIGATION,
# whatever the bead's status says. An insight is closed by design and can still owe them a
# reply.
#
# The candidate list and every comment on it are each read in ONE call — spira/unanswered.py,
# which is also what a test drives directly — never one `bd comments <id>` per candidate: that
# fan-out over hundreds of attention beads is what took this past 10s against a loaded Dolt.
set -uo pipefail
# Every path comes from the harness's one configuration surface. It is two directories
# away because the cockpit ships beside the harness, not inside it.
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/../spira" && pwd -P)/conf.sh"
export BEADS_NO_AUTO_IMPORT=1
COUNT_ONLY=0; [ "${1:-}" = "--count" ] && COUNT_ONLY=1
. "$(dirname "$0")/db.sh"

# Whose voice counts as "answered": the actor the OPERATOR's own comments are recorded under,
# which is a config key because it is one installation's account name. Everything else in the
# thread is mine, and a default of somebody's first name would make every other installation
# read its own replies as an answer.
HUMAN="${COCKPIT_HUMAN:-$SPIRA_OPERATOR_ACTOR}"
UNANSWERED="$(cd "$(dirname "$0")/../spira" && pwd -P)/unanswered.py"

raw=$(cockpit_attention_beads) || exit 1
rows=$(printf '%s' "$raw" | python3 "$UNANSWERED" \
    "bd=$BD" "db=$COCKPIT_DB" "ask_label=$SPIRA_ASK_LABEL" "human=$HUMAN")

n=$(printf '%s' "$rows" | grep -c . || true)
if [ "$COUNT_ONLY" = 1 ]; then printf '%s\n' "${n:-0}"; exit 0; fi
if [ "${n:-0}" -eq 0 ]; then echo "no threads are waiting on a reply"; exit 0; fi
printf '%s waiting on a reply, longest first:\n' "$n"
printf '%s' "$rows" | while IFS=$'\t' read -r id mins ts text; do
    printf '  %-10s %5sm  %s\n' "$id" "$mins" "$text"
done
