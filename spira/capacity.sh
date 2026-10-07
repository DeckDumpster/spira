#!/usr/bin/env bash
#
# capacity.sh — the account's five-hour window: is it shut, and which attempts did it cost?
#
#   capacity.sh status              is a pause in force, and until when
#   capacity.sh scan                every session log whose session the account refused
#   capacity.sh reclassify          what would be given back (changes nothing)
#   capacity.sh reclassify --apply  give it back
#   capacity.sh pause <seconds>     shut the window by hand, for a drill or a known outage
#   capacity.sh resume              lift a pause early
#
# WHY RECLASSIFY IS EVIDENCE-DRIVEN AND NOT A LIST OF IDS
# ------------------------------------------------------
# An attempt charged for an outage is a false attempt and should be given back. The obvious
# way to do that is to name the beads that look worst and subtract — and it is wrong, because
# nothing in the count says WHY it was charged. This bead was filed naming six beads and
# ~177 attempts as capacity-caused on the strength of a `grep` for `429` and `503` in their
# logs; those strings are in the logs, in five-digit token counts (`cache_read_input_tokens`
# ending 429), and not one of them is an HTTP status. Subtracting on that reading would have
# handed attempts back to beads that genuinely failed, and un-poisoned work that is poisoned
# for good reason.
#
# So this reclassifies exactly what it can still SEE: a bead whose surviving session log
# ends in an account refusal gets one attempt back. It refuses to reason about the rest, and
# `scan` prints the same evidence so the refusal is checkable rather than asserted.
#
# THE HORIZON IS ONE ATTEMPT DEEP, and deliberately so. The log keeps every attempt now, but
# what is being asked here is whether THIS bead's latest session was refused, and an older
# segment answers a question about a window that has already reopened — so scan reads only
# the last one, through capacity_reset_at. Earlier segments are there to be read by a person
# asking what happened; they are not evidence about the attempt now on the ladder. The fix
# that matters is still the one in aeon.sh that stops charging the attempt in the first
# place, and this is only the cleanup behind it.
#
# EVIDENCE OUTLIVES THE WITHDRAWAL IT JUSTIFIES, so this keeps a ledger. A log that ends in
# a refusal is still there tomorrow, and a cleanup with no memory of itself reads it as
# grounds for a second withdrawal, then a third — attempt counts walking down to zero, after
# which nothing can ever poison however genuinely it keeps failing. Nothing calls this on a
# timer, which is not a defence: a hand-run cleanup command invites being run again. Each
# withdrawal is therefore marked against the fingerprint of the log that justified it, under
# `$SPIRA_RUN/capacity-withdrawn/<id>`, and a re-run is a no-op until a NEW refusal appends
# to that log. Delete a mark to have its log reconsidered.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

CMD="${1:-status}"; shift 2>/dev/null || true
APPLY=0; [ "${1:-}" = "--apply" ] && APPLY=1

# Every bead whose surviving log shows the account refusing its session, with the epoch the
# window was said to reopen. Printed as "<id> <epoch>" so callers do not re-parse prose.
scan() {
    local f id at
    for f in "$SPIRA_RUN"/*.log; do
        [ -e "$f" ] || continue
        id="$(basename "$f" .log)"
        # The ledger and the harness's own logs live in this directory too and are not
        # session traces. A bead id is what aeon.sh names its log after; anything else here
        # belongs to some other writer.
        case "$id" in aeon-*|sentinel|landing|reflect|collector|beadtable|strands) continue ;; esac
        at="$(capacity_reset_at "$f")" || continue
        printf '%s %s\n' "$id" "$at"
    done
}

case "$CMD" in
status)
    if capacity_paused; then
        printf 'PAUSED  the account is out for another %ss (until %s), because of %s\n' \
            "$SPIRA_CAPACITY_LEFT" \
            "$(date -d "@$(capacity_pause_until)" +%H:%M 2>/dev/null)" \
            "$(capacity_pause_why 2>/dev/null || echo 'an unrecorded session')"
        exit 0
    fi
    printf 'OPEN    no capacity pause is in force\n'
    ;;

scan)
    n=0
    while read -r id at; do
        [ -n "${id:-}" ] || continue
        n=$((n+1))
        printf '%-22s refused, window reopened %s, attempts now %s\n' \
            "$id" "$(date -d "@$at" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')" \
            "$(attempts_of "$id" 2>/dev/null || echo 0)"
    done <<< "$(scan)"
    # ZERO IS A CLAIM AND IT NEEDS A CONTROL. An empty scan and a scan pointed at the wrong
    # directory print the same nothing, so say which directory was read and how many session
    # logs were in it (law-absence-needs-a-positive-control).
    printf -- '--- %s session log(s) read from %s, %s refused\n' \
        "$(ls -1 "$SPIRA_RUN"/*.log 2>/dev/null | wc -l)" "$SPIRA_RUN" "$n"
    ;;

reclassify)
    n=0; already=0; refused=0
    while read -r id at; do
        [ -n "${id:-}" ] || continue
        # THE SAME LOG MAY ONLY BE PAID BACK ONCE. Without this the evidence outlives the
        # withdrawal it justified, so every re-run takes another attempt off the same
        # refusal and the count walks to zero — after which the bead can never poison,
        # however genuinely it goes on failing. A new refusal appends a segment, which moves
        # the fingerprint, and is withdrawn normally. Growth alone cannot pay a log back
        # twice: scan reaches here only while the LAST segment is a refusal, and an attempt
        # that appends anything else takes the log out of scan's answer entirely.
        fp="$(capacity_log_fingerprint "$SPIRA_RUN/$id.log")" || continue
        if [ "$fp" = "$(capacity_withdrawn_fp "$id")" ]; then
            already=$((already+1))
            printf 'ALREADY  %-20s this log was already given back; attempts now %s\n' \
                "$id" "$(attempts_of "$id" 2>/dev/null || echo 0)"
            continue
        fi
        cur="$(attempts_of "$id")"; cur="${cur:-0}"
        [ "$cur" -gt 0 ] || continue
        # Poison is the lifecycle row's hold, never a label (sp-psztcc): `held` exits 0 when
        # the row carries it. Its exit code is read directly — no pipe, so nothing for
        # pipefail to mangle (law-no-grep-q-under-pipefail).
        poisoned=0
        spira-lc held "$id" poison >/dev/null 2>&1 && poisoned=1
        lifts=0
        [ "$poisoned" = 1 ] && [ "$(( cur - 1 ))" -lt "${SPIRA_POISON_AT:-3}" ] && lifts=1
        n=$((n+1))
        if [ "$APPLY" = 1 ]; then
            # Remove the HIGHEST attempt label, because attempts_of reads the maximum: the
            # count is the top of the ladder, not the number of rungs, so taking the top rung
            # away is exactly what "one attempt back" means.
            #
            # THE RESULT IS CHECKED. A removal that failed and still printed RESTORED would
            # be a claim the ledger then makes permanent, and the attempt would stay charged
            # with nothing left saying so.
            #
            # THE RUNG IS FOUND, NEVER RECONSTRUCTED as "sp-attempt-$cur": a rung carries the
            # outcome that charged it (`sp-attempt-2-unlanded`), so the reconstructed name
            # matches no label and the removal withdraws nothing at all.
            rung="$(counter_label "$id" sp-attempt "$cur")" || rung="sp-attempt-$cur"
            if ! bdq label remove "$id" "$rung" >/dev/null 2>&1; then
                printf 'REFUSED  %-20s could not remove %s — left charged, nothing recorded\n' \
                    "$id" "$rung"
                n=$((n-1)); refused=$((refused+1))
                continue
            fi
            bdq note "$id" "Attempt $cur withdrawn: its session was refused by the account for want of capacity, not by anything about this work. Evidence: $SPIRA_RUN/$id.log ends in a rejected rate_limit_event." >/dev/null 2>&1
            # A bead poisoned only by that attempt is no longer poisoned. Its hold goes with
            # it — leaving it would keep the bead out of every fayth's claimable set for a
            # failure that was withdrawn, which is the whole harm this is undoing.
            if [ "$lifts" = 1 ]; then
                spira-lc unhold "$id" poison capacity.sh >/dev/null 2>&1
                printf 'RESTORED %-20s attempt %s withdrawn, poison lifted\n' "$id" "$cur"
            else
                printf 'RESTORED %-20s attempt %s withdrawn\n' "$id" "$cur"
            fi
            # RECORDED ONLY AFTER THE WITHDRAWAL WAS MADE, so an interrupted run repeats one
            # withdrawal at worst rather than recording one it never performed.
            capacity_withdrawn_mark "$id" "$fp" "$cur"
        else
            printf 'would restore %-20s attempt %s -> %s%s\n' "$id" "$cur" "$(( cur - 1 ))" \
                "$( [ "$lifts" = 1 ] && printf ', poison would lift' )"
        fi
    done <<< "$(scan)"
    # ZERO IS A CLAIM AND IT NEEDS A CONTROL. "Nothing to reclassify" is true of a run that
    # found no refusals AND of one whose refusals were all paid back already, and those are
    # different facts about the harness. Say which, and say where the marks are kept, so an
    # operator who genuinely wants a log reconsidered knows what to delete
    # (law-absence-needs-a-positive-control).
    if [ "$n" = 0 ] && [ "$already" = 0 ] && [ "$refused" = 0 ]; then
        printf 'nothing to reclassify — no surviving session log ends in an account refusal\n'
    fi
    printf -- '--- %s to give back, %s already marked; marks in %s\n' \
        "$n" "$already" "$SPIRA_CAPACITY_WITHDRAWN"
    [ "$APPLY" = 1 ] || printf -- '--- dry run; pass --apply to make these changes\n'
    # A run that could not make a withdrawal it identified exits non-zero, so an operator who
    # ran this from a script is told rather than left to read the count.
    [ "$refused" = 0 ] || { printf -- '--- %s withdrawal(s) could not be made\n' "$refused" >&2; exit 1; }
    ;;

pause)
    secs="${1:-900}"
    capacity_pause_set "$(( $(date +%s) + secs ))" "a pause set by hand"
    ;;

resume)
    rm -f "$SPIRA_CAPACITY_PAUSE"
    printf 'the pause is lifted\n'
    ;;

*) printf 'usage: capacity.sh [status|scan|reclassify [--apply]|pause <seconds>|resume]\n' >&2; exit 1 ;;
esac
