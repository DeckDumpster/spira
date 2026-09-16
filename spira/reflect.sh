#!/usr/bin/env bash
#
# reflect.sh — the inference tier. Reached only when every deterministic check has passed
# and the DAG is still not moving.
#
#   reflect.sh "<ids>"
#
# WHY THIS IS SEPARATE AND RARE
# -----------------------------
# The harness is meant to be cheap, quick and frequent; this is none of those. It runs at
# most once an hour, only from sentinel.sh CHECK 8, and only when there is no rule left to
# apply — because if there were a rule, it would already be a check.
#
# Its output is not an action. It writes a diagnosis and, when the fix is one the operator must
# choose between, an escalation carrying a default. Letting it act directly would make the
# expensive tier the one with the most authority, which is backwards: the deterministic
# checks act, and this one explains why they had nothing to do.
#
# WHAT TO DO WITH ITS OUTPUT
# --------------------------
# A stall it diagnoses once should become a deterministic check in sentinel.sh, so this is
# never asked about it again. That is the ladder: custom -> advisory -> statute ->
# mechanism. Inference is where a rule is discovered, not where it lives.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

IDS="${1:-}"
[ -n "$IDS" ] || { log "reflect: nothing to reflect on"; exit 0; }

STATE="$(for id in $IDS; do bdq show "$id" 2>/dev/null | grep -vE '^💡|^warning|^  Fix|^  Or'; echo; done)"
BLOCKED="$(bdq blocked --limit 0 2>/dev/null | grep -E '^\[|sp-' | head -40)"

PROMPT="You are the Spira sentinel's judgement tier. Every deterministic check has passed
and the work graph is still not moving: beads remain open, none are ready, none are
running, and nothing was reclaimed, poisoned, reopened or landed this pass.

Goal epic: $SPIRA_GOAL${SPIRA_DESIGN:+
Design: $SPIRA_DESIGN}

## Open beads

$STATE

## Blocked

$BLOCKED

## Your task

Diagnose why nothing is ready, in at most 150 words. Then answer exactly one question:
is this a gap a program could have detected? If yes, state the deterministic check that
would have caught it, precisely enough to implement in sentinel.sh — that is the valuable
output, because a stall diagnosed by inference twice is a check that was never written.

If the fix needs a decision only $SPIRA_OPERATOR can make, end with a line beginning 'ESCALATE:'
followed by the decision stated as a question with a default. Otherwise end with a line
beginning 'CHECK:' followed by the proposed deterministic check. Do not take any action
and do not modify any bead."

printf '\n===== %s =====\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
out="$(printf '%s' "$PROMPT" | timeout 600 claude -p --model claude-opus-5 \
        --allowedTools "Read,Grep,Glob" --dangerously-skip-permissions 2>&1)"
printf '%s\n' "$out"

# An escalation must reach the cockpit, not just this log. An answer with no delivery path
# is the same as no answer.
if grep -q '^ESCALATE:' <<< "$out"; then
    q="$(grep -m1 '^ESCALATE:' <<< "$out" | sed 's/^ESCALATE: *//')"
    _dflt="$(grep -m1 '^CHECK:' <<< "$out" | sed 's/^CHECK: *//' || echo 'read the diagnosis and decide')"
    # The diagnosis IS the evidence, and it already exists in $out.
    [ -x "$SPIRA_HOME/mail.sh" ] && "$SPIRA_HOME/mail.sh" send operator \
        --from "Reflect <reflect@spira>" \
        --subject "$q" \
        --kind question \
        --default "$_dflt" <<MAILEOF >/dev/null 2>&1 \
        && log "reflect: escalated to the cockpit"
## Question
$q

## Default
$_dflt

the Spira DAG is stalled with open work and nothing ready

$out
MAILEOF
fi
