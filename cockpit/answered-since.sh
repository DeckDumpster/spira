#!/usr/bin/env bash
#
# answered-since.sh — at session start, report what the operator said while nobody was home.
#
# WHY. The live watcher only runs inside a live session. The operator answers from their
# phone and from the cockpit pane at hours when no session exists, and an answer delivered
# to nobody is worse than an unanswered question: they believe they have replied, and the
# next session cheerfully re-asks. One sat answered ("take your default") until they asked
# why nothing had happened.
#
# The SessionStart hook already reports what is still OPEN for them. This is its other half,
# and it covers BOTH ways they speak — a close carrying a verdict, and a comment. The comment
# leg is not a refinement: an FYI is created closed and carries no escalation label, so
# nothing here could ever have seen a reply left on one, and one sat unread until they said
# so. See spira/answers.py, which is also what the live watcher runs.
#
# Prints only what is past the marks, so an answer is announced once rather than every
# session for a day. The marks are written even when there is nothing to say.
set -uo pipefail

. "$(dirname "$0")/db.sh"
# One mark per leg. A comment does not bump the bead's updated_at, so a cursor over closes
# cannot say how far the comment leg has read. Both come from conf.sh (SPIRA_ANSWER_MARK /
# SPIRA_ANSWER_COMMENT_MARK) so they default under SPIRA_RUN rather than inside the harness
# tree; ANSWER_MARK and ANSWER_COMMENT_MARK remain as per-invocation overrides for tests.
VERDICT_MARK="${ANSWER_MARK:-${SPIRA_ANSWER_MARK}}"
COMMENT_MARK="${ANSWER_COMMENT_MARK:-${SPIRA_ANSWER_COMMENT_MARK}}"
ANSWERS="$(cd "$(dirname "$0")/../spira" && pwd -P)/answers.py"

mkdir -p "$(dirname "$VERDICT_MARK")" "$(dirname "$COMMENT_MARK")"

# Narrowed by the server to the labels the attention surface is about — the same rows the
# live watcher reads, and the same rows answers.py would have kept anyway. A session-start
# hook is on the critical path of every session, so the whole bead list is a cost paid by a
# human waiting.
raw=$(cockpit_attention_beads) || exit 0
printf '%s' "$raw" | python3 "$ANSWERS" \
    "bd=$BD" "db=$COCKPIT_DB" \
    "ask_label=$SPIRA_ASK_LABEL" \
    "operator_actor=${SPIRA_OPERATOR_ACTOR:-operator}" \
    "operator=${SPIRA_OPERATOR:-the operator}" \
    "verdict_cursor=$VERDICT_MARK" "comment_cursor=$COMMENT_MARK" \
    "self_closed=${SELF_CLOSED:-${SPIRA_SELF_CLOSED}}" \
    format=session
