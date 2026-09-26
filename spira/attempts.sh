#!/usr/bin/env bash
#
# attempts.sh — read and repair the two counters that decide whether a bead is retried.
#
#   attempts.sh audit                  what every claimable bead is carrying, and why
#   attempts.sh reclassify [--apply]   move rungs that name no cause onto the reclaim counter
#   attempts.sh deadlocked [--apply]   poisoned beads whose work is finished and would merge
#
# WHY THIS EXISTS. The attempt counter feeds the poison threshold, and poison takes a bead
# out of circulation permanently. It was default-ALLOW — anything a short list of exemptions
# did not positively excuse was charged as a failure of the WORK — so it recorded worker
# deaths, rate-limit refusals and a worktree deleted under a live aeon as evidence about
# beads nobody had looked at. Counts in the tens accumulated against a threshold of three.
#
# The rule is now default-DENY, and a rung records the outcome that charged it (lib.sh,
# session_outcome / outcome_charges). This tool applies the same rule BACKWARDS: a rung
# carrying no cause predates the requirement to name one, and an unnamed rung is not
# evidence. That is not an amnesty, it is the same statute read in the only direction it can
# be read — UNKNOWN does not charge.
#
# `reclassify` WILL NOT LIFT A POISON. Clearing a false COUNT is arithmetic and needs nobody's
# permission; re-queueing a bead the operator has an open decision about is a different act on
# the strength of "these rungs named no cause", which says nothing about whether the work is
# any good. The label is left exactly where it is and the audit says so.
#
# `deadlocked` is the one command that does lift it, and it does so on much stronger evidence:
# not that the count was wrong, but that the WORK IS FINISHED. A poisoned bead stays open, no
# persona may claim an open bead carrying the label, and the landing pass lands only a closed
# bead — so a poisoned bead whose branch names it and merges cleanly into the base is finished,
# landable work that nothing will ever pick up again. That is a deadlock reached by counting,
# and it is not a judgement about the approach: there is nothing left to judge.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

APPLY=0
CMD="${1:-audit}"; shift 2>/dev/null || true
for a in "$@"; do [ "$a" = "--apply" ] && APPLY=1; done

# EVERY BEAD A PERSONA COULD CLAIM, not the goal epic's children: fences bind on a partition's
# labels and not on ancestry, which is the asymmetry that let large counts accumulate unseen.
#
# The partitions come from the chamber rather than from a literal here, so this tool and the
# summoner cannot come to disagree about which beads are ours. Their EXCLUSIONS are
# deliberately dropped: a poisoned bead is excluded from dispatch and is exactly the bead
# whose count most needs reading, so filtering it out here would hide the evidence.
#
# `--limit 0` because `bd list` silently truncates at 50, which would restore the very defect
# this set exists to close, with a different boundary.
candidates() {
    local labels exclude
    while IFS=$'\t' read -r labels exclude; do
        [ -n "$labels" ] || continue
        bdjson list --status open,in_progress --limit 0 --label "$labels" 2>/dev/null \
            | python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d,list) else [d]): print(i["id"])' 2>/dev/null
    done < <(fayth_partitions)
    return 0
}
uniq_candidates() { candidates | awk 'NF && !seen[$0]++'; }

# Read the labels once and match a herestring: piping into `grep -q` closes the pipe on the
# first match and SIGPIPEs the writer, which pipefail reports as failure — so the test would
# read FALSE exactly when it succeeded (law-no-grep-q-under-pipefail).
poisoned() { local l; l="$(bdq label list "$1" 2>/dev/null)" || l=""; grep -q spira-poison <<<"$l"; }

case "$CMD" in
audit)
    n=0; total=0
    for id in $(uniq_candidates); do
        total=$((total+1))
        a="$(attempts_of "$id")"; a="${a:-0}"
        r="$(reclaims_of "$id")"; r="${r:-0}"
        q="$(requeues_of "$id")"; q="${q:-0}"
        [ "$a" != 0 ] || [ "$r" != 0 ] || [ "$q" != 0 ] || continue
        n=$((n+1))
        printf '%-20s attempts=%-3s reclaims=%-3s requeues=%-3s %s\n' "$id" "$a" "$r" "$q" \
            "$(poisoned "$id" && echo POISONED || echo '')"
        counter_causes "$id" sp-attempt | sed 's/^/    attempt /'
        counter_causes "$id" sp-reclaim | sed 's/^/    reclaim /'
        counter_causes "$id" sp-requeue | sed 's/^/    requeue /'
    done
    # ZERO IS A CLAIM AND IT NEEDS A CONTROL. "No bead carries a counter" and "the query
    # returned nothing" print the same nothing, and the second reads as all-clear
    # (law-absence-needs-a-positive-control). The denominator is what tells them apart.
    printf -- '--- %s bead(s) carrying counters, of %s claimable\n' "$n" "$total"
    ;;

reclassify)
    n=0
    for id in $(uniq_candidates); do
        keep=""; drop=0
        while read -r rung cause; do
            [ -n "${rung:-}" ] || continue
            if [ "$cause" = unrecorded ]; then drop=$((drop+1)); else keep="$keep $cause"; fi
        done <<< "$(counter_causes "$id" sp-attempt)"
        [ "$drop" -gt 0 ] || continue
        n=$((n+1))
        was="$(attempts_of "$id")"; was="${was:-0}"
        now="$(printf '%s' "$keep" | wc -w)"
        if [ "$APPLY" != 1 ]; then
            printf 'would reclassify %-20s attempts %s -> %s, %s rung(s) onto the reclaim counter%s\n' \
                "$id" "$was" "$now" "$drop" \
                "$(poisoned "$id" && printf ' (POISONED — the label is left alone)')"
            continue
        fi
        # THE LADDER IS ITS TOP RUNG, NOT ITS RUNG COUNT: attempts_of reads the maximum, so
        # removing a rung from the middle would leave a gap that still reads as the old
        # number and the withdrawal would be invisible in the one number the threshold
        # consults. Every attempt rung comes off and the survivors go back on renumbered.
        while read -r rung cause; do
            [ -n "${rung:-}" ] || continue
            lbl="$(counter_label "$id" sp-attempt "$rung")" || lbl="sp-attempt-$rung"
            bdq label remove "$id" "$lbl" >/dev/null 2>&1
        done <<< "$(counter_causes "$id" sp-attempt)"
        i=0
        for cause in $keep; do i=$((i+1)); bdq label add "$id" "sp-attempt-$i-$cause" >/dev/null 2>&1; done
        j=0
        while [ "$j" -lt "$drop" ]; do j=$((j+1)); bump_reclaim "$id" unrecorded >/dev/null; done
        bdq note "$id" "Attempt counter reclassified: $drop of $was rung(s) named no outcome, so they were moved onto the reclaim counter and no longer feed poison. A rung that does not say what the work did wrong is not evidence that the work is wrong — it predates the rule that a charge must name its cause, and the large counts on this board were accumulated by dead heartbeats, rate-limit refusals and worktrees deleted under live aeons. Attempts now $now." >/dev/null 2>&1
        printf 'RECLASSIFIED %-20s attempts %s -> %s, %s onto reclaim%s\n' "$id" "$was" "$now" "$drop" \
            "$(poisoned "$id" && printf ' (still POISONED — the label is the operator'\''s to lift)')"
    done
    [ "$n" = 0 ] && printf 'nothing to reclassify — every attempt rung on the board names its cause\n'
    [ "$APPLY" = 1 ] || printf -- '--- dry run; pass --apply to make these changes\n'
    ;;

prune-reclaims)
    # Strip sp-reclaim-N-unrecorded labels from named beads. These accumulate when sessions die
    # within seconds — before they can record their own exit — and strand.sh ghost-reclaims
    # them. The reclaim counter is diagnostic and feeds nothing that stops a bead being worked,
    # but dozens of unrecorded rungs make bd show and label views unreadable and cause any
    # logic that counts reclaims to flag the bead as troubled.
    #
    # Pass bead IDs as arguments. With no arguments the command prints usage and exits: the
    # cleanup is targeted rather than a sweep, because this tool must not strip a named-cause
    # rung by accident. DO NOT pass --all to work around this; the distinction exists so a
    # future caller who gets the flag wrong cannot silently destroy the reclaim record.
    #
    # DO NOT touch sp-reclaim-N labels without the unrecorded suffix: those carry a named
    # cause (ghost, killed, etc.) and record real information about the worker.
    ids=()
    for a in "$@"; do [ "$a" != "--apply" ] && ids+=("$a"); done
    if [ "${#ids[@]}" -eq 0 ]; then
        die "usage: attempts.sh prune-reclaims <id> [<id>...] [--apply]"
    fi
    n=0
    for id in "${ids[@]}"; do
        # Read all labels, capture before matching — piping into grep -q closes the pipe early
        # and SIGPIPEs the writer under pipefail (law-no-grep-q-under-pipefail).
        all="$(bdq label list "$id" 2>/dev/null | sed -n 's/^ *- //p')" || all=""
        mapfile -t to_remove < <(grep -xE 'sp-reclaim-[0-9]+-unrecorded' <<<"$all" || true)
        count="${#to_remove[@]}"
        [ "$count" -gt 0 ] || continue
        n=$((n+1))
        if [ "$APPLY" != 1 ]; then
            printf 'would prune %-20s %s sp-reclaim-N-unrecorded label(s)\n' "$id" "$count"
            continue
        fi
        for lbl in "${to_remove[@]}"; do
            bdq label remove "$id" "$lbl" >/dev/null 2>&1
        done
        bdq note "$id" "Stripped $count sp-reclaim-N-unrecorded label(s). Each was written by a ghost-reclaim when an aeon session was terminated by rate-limiting in under ten seconds, before it could record its own exit; they describe infrastructure noise, not the work. The reclaim counter feeds nothing that stops a bead being worked, but at this count it made bd show and label views unreadable and caused reclaim-counting logic to flag the bead as troubled. sp-39c." >/dev/null 2>&1
        printf 'PRUNED %-20s %s sp-reclaim-N-unrecorded label(s) removed\n' "$id" "$count"
    done
    [ "$n" = 0 ] && printf 'nothing to prune — no named beads carry sp-reclaim-N-unrecorded labels\n'
    [ "$APPLY" = 1 ] || printf -- '--- dry run; pass --apply to remove these labels\n'
    ;;

deadlocked)
    # THE PREDICATE IS THE COMMIT GRAPH, NOT THE COUNTER. Beads damaged before a rung carried
    # its cause have nothing in their counts saying why they were charged, so a sweep reasoning
    # from the counts would un-poison work that is poisoned for good reason. This asks the only
    # question the deadlock itself poses, and both halves are required: a branch that merges
    # cleanly but carries nothing naming the bead is an empty branch, and one that names the
    # bead but conflicts is work a person still has to finish.
    n_seen=0; n_hit=0; n_done=0
    for id in $(uniq_candidates); do
        poisoned "$id" || continue
        n_seen=$((n_seen+1))
        why=""
        r_name="$(bead_repo "$id")"; r_path="$(repo_root "$r_name")" || r_path=""
        br="spira/$id"
        if [ -z "$r_path" ]; then
            why="repo:$r_name is not in the repo map — cannot look at its branch"
        elif ! git -C "$r_path" show-ref --verify -q "refs/heads/$br"; then
            why="no branch $br in $r_name — nothing was committed"
        else
            refs="$(spira_landrefs "$r_path")" || refs=""
            base="${refs%% *}"
            if [ -z "$base" ]; then
                why="$r_name cannot say what it lands on — not judging its branch"
            else
                # Captured whole and matched with a herestring: `git log | grep -q` under
                # pipefail returns 141 on a MATCH (law-no-grep-q-under-pipefail).
                subjects="$(git -C "$r_path" log --format='%s%n%b' -n 200 "$br" 2>/dev/null)"
                if ! grep -qF "$id" <<<"$subjects"; then
                    why="$br exists but no commit on it names $id"
                elif ! git -C "$r_path" merge-tree --write-tree "$base" "$br" >/dev/null 2>&1; then
                    why="$br does not merge into $base — a person has to resolve it"
                fi
            fi
        fi
        if [ -n "$why" ]; then
            printf 'KEEP     %-20s %s\n' "$id" "$why"
            continue
        fi
        n_hit=$((n_hit+1))
        if [ "$APPLY" != 1 ]; then
            printf 'WOULD    %-20s finished on %s and it merges into %s — would lift the poison\n' \
                "$id" "$br" "$base"
            continue
        fi
        # THE POISON ONLY. The rungs stay exactly where they are: they are the record of what
        # happened to this bead and the reason its escalation was raised. Taking the label off
        # does not touch the count feeding the threshold — it is read from the events trail,
        # not the label — so CHECK 4 would otherwise see the same n against no label and
        # re-poison within its next pass. poison_lifted_mark records the count this lift
        # happened at; check4_decide will not re-poison as long as nothing has moved past it,
        # and a genuinely new failure (one more claim after this lift) still poisons it again.
        if bdq label remove "$id" spira-poison >/dev/null 2>&1; then
            bdq note "$id" "Poison lifted by attempts.sh deadlocked: $br carries a commit naming $id and merges cleanly into $base, so this is finished, landable work. A poisoned bead stays open, an open bead carrying the label is claimed by nobody, and the landing pass lands only closed beads — so the label was holding completed work out of the queue permanently. The counters are left standing as the record of how it got here." >/dev/null 2>&1
            att="$(attempts_of "$id")"; poison_lifted_mark "$id" "${att:-0}" || true
            n_done=$((n_done+1))
            printf 'RESTORED %-20s poison lifted; %s is finished and merges into %s\n' "$id" "$br" "$base"
        else
            printf 'REFUSED  %-20s the poison label would not come off\n' "$id"
        fi
    done
    # ZERO IS A CLAIM AND IT NEEDS A CONTROL: an empty sweep and a sweep against a database it
    # could not read print the same nothing (law-absence-needs-a-positive-control).
    printf -- '--- %s poisoned bead(s) examined, %s finished and landable, %s restored\n' \
        "$n_seen" "$n_hit" "$n_done"
    [ "$APPLY" = 1 ] || printf -- '--- dry run; pass --apply to lift these\n'
    ;;

*) die "usage: attempts.sh audit | reclassify [--apply] | prune-reclaims <id>... [--apply] | deadlocked [--apply]" ;;
esac
