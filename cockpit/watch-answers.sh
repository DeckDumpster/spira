#!/usr/bin/env bash
#
# watch-answers.sh — tell the session when the operator answers something.
#
#   watch-answers.sh once    one pass; prints nothing when nothing is new
#   watch-answers.sh loop    the same, on an interval — the default, and the daemon form
#
# WHY THIS EXISTS. The operator answered an escalation in the attention pane with "take your
# default" and the session never noticed. They had to ask, hours later, whether a notification
# job was missing. It was: a panel rewrite made the bead the single source of truth — `bd
# close --reason` IS the verdict now, which is the better data model — and deleted the write
# to the log a session used to tail, with nothing replacing the notification leg.
#
# The lesson is the one that keeps recurring here: an escalation queue has two halves, the ask
# and the answer, and only the ask had a mechanism. A verdict that reaches nobody is worse
# than an unanswered question, because the decider believes they replied.
#
# Emits one line per event, so it is equally a Monitor command and a `watchd` daemon row:
#   Monitor({command: '<cockpit>/watch-answers.sh', persistent: true})
#
# THIS FILE DECIDES ONLY WHICH DATABASE AND WHERE THE MARKS LIVE. Both legs — a close carrying
# a verdict, and a comment on a bead that can never be closed again — are answers.py, which
# cockpit/answered-since.sh also runs. One implementation rather than two: the arrangement
# this replaces had a close-watcher here and a different one beside it, and the blindness to a
# comment on an FYI had to be found separately in each. Keeping the superseded one standing is
# what let two sessions attach the blind one.
set -uo pipefail

. "$(dirname "$0")/db.sh"
# THE WITNESS: proof this watcher can SEE, kept apart from how far it has read. Every pass
# writes the ids its query returned here, and the manifest's health assertion greps it for one
# of ours — a watcher reading a database that was retired underneath it holds rows, just not
# ours, and is otherwise indistinguishable from a watcher with nothing to say.
#
# It is deliberately not the cursors below. A cursor names a bead only in the instant one was
# reported, so an assertion over it would read DEGRADED on a healthy quiet watcher, which is
# the expensive kind of alarm (law-alerts-must-be-actionable). Coupling the two is also what
# made the original blind: sight and position lived in one snapshot, so clearing a poisoned
# one took the proof with it.
#
# WHERE IT LIVES IS CONFIGURATION. The path is known here and in the assertion that reads it,
# and those two disagreeing is a permanent DEGRADED against a watcher working perfectly.
# ANSWER_STATE stays ahead of it so a test can hand this script a scratch file.
WITNESS="${ANSWER_STATE:-${SPIRA_ANSWER_STATE}}"
INTERVAL="${ANSWER_POLL:-45}"

# TWO MARKS, because a close and a comment are ordered by different clocks: a comment does NOT
# bump the bead's updated_at, so a cursor over closes cannot express how far the comment leg
# has read. One file for one question — and they cover each other, since answers.py treats a
# mark missing while its sibling survives as CLEARED rather than new, and takes the sibling's
# position instead of silently seeding at now.
#
# Under SPIRA_RUN, the harness's own runtime directory, rather than inside the shipped tree:
# these are per-installation state, and they are deliberately not the marks answered-since.sh
# keeps. That hook and this watcher both report, so a shared mark would mean whichever ran
# first swallowed the answer for the other.
mkdir -p "$SPIRA_RUN" "$(dirname "$WITNESS")"
VERDICT_CURSOR="${VERDICT_CURSOR:-$SPIRA_RUN/.verdict-cursor}"
COMMENT_CURSOR="${COMMENT_CURSOR:-$SPIRA_RUN/.comment-cursor}"
# ANSWERS_BIN is the seam a suite drives to test this loop's own contract (does it print, does
# it wake, does it exit 1 on a failing pass) without paying for a real bd round trip through
# the actual answers.py on every case.
ANSWERS="${ANSWERS_BIN:-$(cd "$(dirname "$0")/../spira" && pwd -P)/answers.py}"

emit() {
    # NARROWED BY THE SERVER. This ran every 45 seconds against every bead in the database and
    # threw all but a twentieth of them away one line later; cockpit_attention_beads asks for
    # the labels the attention surface is actually about. A read that fails is NOT an empty
    # database — hold silence rather than render a broken check as all-clear.
    local raw
    raw=$(cockpit_attention_beads) || return 0
    # Run through its own shebang rather than a hardcoded `python3 "$ANSWERS"`, so ANSWERS_BIN
    # can point at a non-Python stub for a loop test that has no need of the real logic.
    printf '%s' "$raw" | "$ANSWERS" \
        "bd=$BD" "db=$COCKPIT_DB" \
        "ask_label=$SPIRA_ASK_LABEL" \
        "operator_actor=${SPIRA_OPERATOR_ACTOR:-operator}" \
        "operator=${SPIRA_OPERATOR:-the operator}" \
        "verdict_cursor=$VERDICT_CURSOR" "comment_cursor=$COMMENT_CURSOR" \
        "witness=$WITNESS" \
        "self_closed=${SELF_CLOSED:-${SPIRA_SELF_CLOSED}}" \
        format=monitor
}

# The reader's only delivery path: a Monitor lease expires, a typed prompt does not.
wake() {
    [ -n "${SPIRA_WAKE:-}" ] || return 0
    # shellcheck disable=SC2086  # SPIRA_WAKE is a command line, split on purpose
    $SPIRA_WAKE "New answers from ${SPIRA_OPERATOR:-the operator}. Run $(cd "$(dirname "$0")/../spira" && pwd -P)/watchd.sh drain answers and act on every verdict it prints." >/dev/null 2>&1 \
        || echo "watch-answers: SPIRA_WAKE refused — these answers reach no reader until the page" >&2
}

case "${1:-loop}" in
    once) emit ;;
    loop) while true; do
              out="$(emit)" || exit 1
              [ -n "$out" ] || { sleep "$INTERVAL"; continue; }
              printf '%s\n' "$out"
              wake
              sleep "$INTERVAL"
          done ;;
    *) echo "usage: watch-answers.sh [once|loop]" >&2; exit 2 ;;
esac
