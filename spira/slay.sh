#!/usr/bin/env bash
#
# slay.sh — stop one aeon cleanly and make its bead say what is true.
#
#   slay.sh --bead <id> [--why "<text>"] [--keep-work] [--close "<reason>" | --reopen]
#
# NAMED ARGUMENTS ONLY — there are no positionals, and a bare word is a usage error. The
# old form took the bead id positionally beside three flags that each carry a value, and
# `slay.sh sp-gjpc "blocked on the P0 fixes"` therefore read the SENTENCE as the bead id
# (2026-09-13). `--bead` cannot be confused with a reason. `-h` lists every argument.
#
# "RETIRE", NOT "NUKE" — the default does not lose work, and the old one-word summary saying
# it did caused an operator to reach for --keep-work in an incident and pass the reason as a
# second positional to get it (2026-09-13). What the default does, in step 5: uncommitted
# work is salvaged to $SPIRA_RUN/reaped, a branch carrying commits the base does not have is
# parked at refs/slain/<id> (a real ref, survives gc), and the deletion is REFUSED outright
# if that parking fails. Only a branch with nothing the base lacks is simply removed. Use
# --keep-work when you want the branch to stay claimable-looking in refs/heads; do not use
# it out of fear of losing the commits.
#
# WHAT "CLEANLY" MEANS, in order, because each step is the one the next depends on:
#   1. A MARKER FIRST, so the aeon's own exit path knows it was slain. aeon.sh charges an
#      attempt on any non-zero exit — a kill is 143 — and three attempts poison a bead. An
#      operator stopping an aeon is not evidence the bead is hard, so the marker makes the
#      exit path release the bead with no attempt charged, the way a spent capacity window
#      already does.
#   2. THE UNIT, NOT THE PID. An aeon is a transient systemd unit with KillMode=control-group,
#      so stopping the unit takes the session, its heartbeat and every child with it. A bare
#      kill of the runner leaves the model session running headless. The pid is the fallback
#      for an aeon that has no unit (a fixture, a hand-run one), and it is matched from the
#      unit's MainPID — never `pkill -f`, which nominates the caller too (law-pgrep-nominates).
#   3. WAIT FOR THE EXIT PATH TO FINISH. The pid file is removed by aeon.sh's own cleanup,
#      after it has released the bead; the pid file going away is the signal that the bead is
#      now ours to set, not a hint that the process is dead.
#   4. THE BEAD SAYS WHAT IS TRUE. Unassigned always — `bd ready --claim` skips an assigned
#      bead while `bd ready` still lists it, which left seven reopened beads unclaimable for
#      hours. Open by default; closed with a reason on request; the `branch:` label comes
#      off with the branch.
#   5. THE WORK, THROUGH lib.sh AND NOWHERE ELSE. Salvage first — a patch of anything
#      uncommitted lands in $SPIRA_RUN/reaped, the same insurance the reaper carries — then
#      the rebase in progress is aborted and spira_destroy_worktree / spira_destroy_branch
#      do the removing, with the tip sha written into the bead's note. Deleted is recoverable
#      from the reflog for a month; a note without the sha is not. Those two primitives are
#      the only permitted callers of `git worktree remove` and `git branch -D` in the
#      harness, and they refuse while anybody is home — which is why step 4's release runs
#      ahead of this rather than after it.
#   6. SAY WHAT WAS DONE, with the evidence, and exit non-zero on anything it could not do.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

usage() {
    cat <<'USAGE'
slay.sh — stop one aeon cleanly and make its bead say what is true.

  slay.sh --bead <id> [--why "<text>"] [--keep-work] [--close "<reason>" | --reopen]

REQUIRED
  --bead <id>        The bead whose aeon is to be stopped. Must exist in SPIRA_DB;
                     an id no bead carries is refused rather than acted on.

OPTIONAL
  --why "<text>"     What the note on the bead records as the operator's reason.
                     Default: "slain by the operator". This is where a sentence goes.
  --keep-work        Leave the branch and worktree exactly as they are.
  --close "<reason>" Close the bead with this reason instead of releasing it.
  --reopen           Release the bead as open and unassigned. This is the default.
  -h, --help         This text.

WHAT HAPPENS TO THE WORK WITHOUT --keep-work
  It is RETIRED, not lost. Uncommitted changes are salvaged to SPIRA_RUN/reaped as a
  patch; a branch carrying commits the base does not have is parked at refs/slain/<id>,
  a real ref that survives gc; and if that parking fails the deletion is REFUSED. Only a
  branch holding nothing the base lacks is simply removed. Use --keep-work when you want
  the branch left in refs/heads, not because you fear losing the commits.

EXIT
  0  the aeon was stopped and the bead says what is true
  1  something could not be done — the message names it
  2  usage: no --bead, an unknown flag, a positional argument, two --bead values,
     or a bead id the store does not carry
USAGE
}

ID=""; MODE=reopen; REASON=""; KEEP=0; WHY="slain by the operator"
while [ $# -gt 0 ]; do
    case "$1" in
        --bead)      [ -z "$ID" ] || { printf 'slay.sh: --bead given twice (%s, %s)\n' "$ID" "${2:-}" >&2; exit 2; }
                     ID="${2:?--bead needs a bead id}"; shift ;;
        --close)     MODE=close; REASON="${2:?--close needs a reason}"; shift ;;
        --reopen)    MODE=reopen ;;
        --keep-work) KEEP=1 ;;
        --why)       WHY="${2:?--why needs text}"; shift ;;
        -h|--help)   usage; exit 0 ;;
        -*)          printf 'slay.sh: unknown flag %s\n' "$1" >&2; usage >&2; exit 2 ;;
        # NO POSITIONALS. A bare word here used to become the bead id, so a reason written
        # beside the id silently replaced it and the run proceeded against a bead named
        # after the sentence, reporting success (2026-09-13).
        *)           printf 'slay.sh: unexpected argument "%s" — this tool takes named arguments only.\n' "$1" >&2
                     printf 'slay.sh: the bead goes in --bead, a reason goes in --why.\n' >&2
                     exit 2 ;;
    esac
    shift
done
[ -n "$ID" ] || { printf 'slay.sh: --bead is required\n' >&2; usage >&2; exit 2; }
fail=0
say() { printf '%s\n' "$*"; }

# ---- 0. THE BEAD MUST EXIST ------------------------------------------------------------
# Step 6 says this script exits non-zero on anything it could not do. It did not: given an id
# no bead carries, it wrote a .slain marker, reported `bead: None status=None`, printed
# `slain: <id>` and exited 0 (2026-09-13). A destructive operator tool that reports success
# for a target it never found is the failure mode every other fence here exists to prevent.
#
# COMPARE THE ID THAT COMES BACK, never just `bd show`'s exit status: `bd show sp-ofeb`
# resolves an orphaned child `sp-ofeb.1` by PREFIX and answers as though the parent existed.
_slay_found="$(bdq show "$ID" --json 2>/dev/null | python3 -c '
import json,sys
try:
    d = json.load(sys.stdin)
    d = d[0] if isinstance(d, list) else d
    print(d.get("id",""))
except Exception:
    print("")' 2>/dev/null)"
[ "$_slay_found" = "$ID" ] || {
    printf 'slay.sh: no bead %s in %s — refusing to act\n' "$ID" "${SPIRA_DB:-?}" >&2
    [ -n "$_slay_found" ] && printf 'slay.sh: the store answered with %s instead (prefix match)\n' "$_slay_found" >&2
    exit 2
}

# ---- 1. the marker ---------------------------------------------------------------------
printf '%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$WHY" > "$SPIRA_RUN/$ID.slain"

# ---- 2. the holder (aeon OR manual hold) -----------------------------------------------
# A hold pidfile means a non-aeon actor (brain session, concierge, hand-run tool) claimed
# this bead via hold.sh. It has no systemd unit and no aeon.sh cleanup trap, so slay just
# kills the heartbeat (recorded in the .hb companion) and removes the pidfile.
hpf="$SPIRA_RUN/hold-$ID.pid"
if [ -f "$hpf" ]; then
    hpid="$(cat "$hpf" 2>/dev/null)"
    hbpid=""; [ -f "${hpf%.pid}.hb" ] && hbpid="$(cat "${hpf%.pid}.hb" 2>/dev/null)"
    [ -n "$hbpid" ] && kill "$hbpid" 2>/dev/null
    rm -f "$hpf" "${hpf%.pid}.hb"
    say "hold: manual hold released for $ID (holder pid ${hpid:-?}, heartbeat ${hbpid:-none})"
    rm -f "$SPIRA_RUN/$ID.slain"
else
    pf="$(ls "$SPIRA_RUN"/aeon-*-"$ID".pid 2>/dev/null | head -1)"
    pid=""; [ -n "$pf" ] && pid="$(cat "$pf" 2>/dev/null)"
    name=""; [ -n "$pf" ] && name="$(cat "${pf%.pid}.name" 2>/dev/null)"
    unit=""
    if [ -n "$pid" ] && command -v systemctl >/dev/null 2>&1; then
        for u in $(systemctl --user list-units 'spira-aeon-*' --no-legend 2>/dev/null | awk '{print $1}'); do
            [ "$(systemctl --user show -p MainPID --value "$u" 2>/dev/null)" = "$pid" ] && { unit="$u"; break; }
        done
    fi
    if [ -z "$pid" ] || [ ! -d "/proc/$pid" ]; then
        say "aeon: none running for $ID (no live pid file) — setting the bead and the work only"
        rm -f "$SPIRA_RUN/$ID.slain"
        # A pid file whose process is gone is litter from an aeon that died without its cleanup;
        # aeon_count removes the same litter, and leaving it here fails the verify below.
        [ -n "$pf" ] && rm -f "$pf" "${pf%.pid}.name"
    else
        if [ -n "$unit" ]; then
            say "aeon: ${name:-?} pid $pid is $unit — stopping the unit"
            systemctl --user stop "$unit" 2>/dev/null || { say "aeon: systemctl stop failed — sending TERM to $pid"; kill -TERM "$pid" 2>/dev/null; }
        else
            say "aeon: ${name:-?} pid $pid has no unit — sending TERM"
            kill -TERM "$pid" 2>/dev/null
        fi
        # ---- 3. wait for aeon.sh's own cleanup: it removes the pid file last -------------------
        t0=$(date +%s)
        while [ -e "$pf" ] || [ -d "/proc/$pid" ]; do
            sleep 1
            if [ $(( $(date +%s) - t0 )) -ge 60 ]; then
                say "aeon: still alive after 60s — KILL"
                kill -KILL "$pid" 2>/dev/null; sleep 1
                rm -f "$pf" "${pf%.pid}.name"
                break
            fi
        done
        [ -d "/proc/$pid" ] && { say "aeon: pid $pid SURVIVED a KILL — investigate by hand"; fail=1; } \
                            || say "aeon: stopped ($(( $(date +%s) - t0 ))s)"
    fi
    rm -f "$SPIRA_RUN/$ID.slain"
fi

# ---- 4a. release the claim, BEFORE anything is destroyed --------------------------------
# A LIVE CLAIM IS ANOTHER ACTOR'S, and bd refuses to overwrite one without being told the
# claim is abandoned — which is exactly what this script has just made true. unclaim is the
# release the aeon itself would have performed; --force on update is the fallback for a bead
# whose claim survived its own exit path.
#
# IT HAPPENS HERE, AHEAD OF THE WORK, because the destruction below goes through lib.sh and
# lib.sh asks `spira_holder_witnesses` whether anybody is home. That predicate reads
# `in_progress` as "the lease has not been released" and refuses — correctly, for every other
# caller. Slaying is the one path that has just MADE the answer no: the aeon is stopped and
# its exit path has run. So the release is the last step of stopping the aeon, not the first
# step of tidying up after it, and doing it in the other order is what left this script
# reaching around the chokepoint with a raw `git worktree remove` and `git branch -D`.
status_of() { bdjson show "$ID" | python3 -c 'import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("status","") if d else "")' 2>/dev/null; }
st="$(status_of)"
if [ "$st" = in_progress ]; then
    # bdq unclaim releases the lease but does NOT change status — the bead stays in_progress,
    # and spira_holder_witnesses reads in_progress as "held", so the worktree/branch removal
    # below is refused. Reopen first: it moves the status to open, making the holder check
    # see nobody home, which is what is actually true at this point.
    # THROUGH bead_reopen, LIKE EVERY OTHER REOPEN. bead_reopen clears the assignee as well
    # as changing the status, so the unclaim below is redundant but harmless; keeping it as
    # a belt-and-suspenders fallback costs nothing and the belt is already documented above.
    bead_reopen "$ID"
    bdq unclaim "$ID" --force >/dev/null 2>&1 \
        || bdq update "$ID" --status open --assignee "" >/dev/null 2>&1 \
        || true
fi
bdq update "$ID" --assignee "" --force >/dev/null 2>&1 || bdq update "$ID" --assignee "" >/dev/null 2>&1

# ---- 5. the work (the sha is read before the branch goes, so the note can carry it) ------
# EVERY DELETION GOES THROUGH lib.sh. The two primitives re-check the witnesses, salvage,
# and write both sides of the act to the reap log — which is the whole point of the
# chokepoint, and which slay.sh most needs: it deletes a live aeon's worktree, with
# uncommitted work in it, at the moment that aeon has just been killed.
repo_name="$(bead_repo "$ID" 2>/dev/null)"; repo=""
[ -n "$repo_name" ] && repo="$(repo_root "$repo_name" 2>/dev/null)"
br="spira/$ID"; wt="$SPIRA_RUN/worktree/$ID"; tip=""; nuked=""; saved=""; kept_msg=""; branch_kept=0
if [ -n "$repo" ] && git -C "$repo" show-ref --verify -q "refs/heads/$br"; then
    tip="$(git -C "$repo" rev-parse --short "$br")"
fi
if [ "$KEEP" = 1 ]; then
    say "work: kept — branch $br${tip:+ at $tip}, worktree $wt"
elif [ -n "$repo" ]; then
    if [ -d "$wt" ]; then
        git -C "$wt" rebase --abort >/dev/null 2>&1 || true
        git -C "$wt" merge --abort  >/dev/null 2>&1 || true
        # Salvaged here rather than left to the primitive so the PATH can be reported and
        # carried into the bead's note. `salvage` resets SALVAGED on entry and the primitive
        # calls it again — finding nothing left to save, which is a success — so the name is
        # taken now or it is lost.
        if salvage "$ID" "$wt"; then
            saved="${SALVAGED:-}"
            [ -n "$saved" ] && say "work: uncommitted changes salvaged to $saved"
        else
            say "work: could not salvage $wt — leaving it in place"; fail=1
        fi
        # WIP COMMIT: commit any remaining uncommitted work onto the branch before the
        # worktree goes. The patch above is the belt-and-braces copy; the commit is the
        # primary record — it is part of the git history and named in the next aeon's brief.
        # Runs regardless of salvage outcome so the commit is independent of the patch.
        if git -C "$wt" status --porcelain 2>/dev/null | grep -q .; then
            git -C "$wt" add -A >/dev/null 2>&1 || true
            if GIT_AUTHOR_NAME="${GIT_AUTHOR_NAME:-spira-slay}" \
               GIT_AUTHOR_EMAIL="${GIT_AUTHOR_EMAIL:-slay@spira.local}" \
               GIT_COMMITTER_NAME="${GIT_COMMITTER_NAME:-spira-slay}" \
               GIT_COMMITTER_EMAIL="${GIT_COMMITTER_EMAIL:-slay@spira.local}" \
               git -C "$wt" commit -q -m "$ID: wip — salvaged at slay ($WHY)" >/dev/null 2>&1; then
                tip="$(git -C "$repo" rev-parse --short "$br" 2>/dev/null || echo ?)"
                say "work: uncommitted changes committed as wip on $br (now at ${tip:-?})"
            else
                say "work: could not make wip commit on $br — the patch is the backup"
            fi
        fi
        if [ "$fail" = 0 ]; then
            if spira_destroy_worktree "$ID" "$wt" "$repo" "slain: $WHY"; then
                say "work: worktree $wt removed"
            else
                say "work: could not remove $wt — see $SPIRA_RUN/reap.log"; fail=1
            fi
        fi
    fi
    if [ -n "$tip" ] && [ "$fail" = 0 ]; then
        parked=""
        if base="$(spira_landref "$repo" 2>/dev/null)"; then
            ahead="$(git -C "$repo" rev-list --count "$base..$br" 2>/dev/null || echo 0)"
            if [ "$MODE" = reopen ] && [ "${ahead:-0}" -gt 0 ]; then
                # Branch has commits the base does not — leave it in refs/heads so the next
                # attempt resumes from it rather than starting from the base. The wip commit
                # (if any) is part of that history and the next aeon's brief names it.
                kept_msg="branch $br kept in refs/heads at $tip for the next attempt"
                say "work: $kept_msg"
                branch_kept=1
            else
                # mode=close, OR branch is zero-ahead (nothing to resume). Park unique work
                # first so refs/slain holds the history before we delete refs/heads.
                if [ "${ahead:-0}" -gt 0 ] && ! content_landed "$repo" "$br" "$base" 2>/dev/null; then
                    # refs/slain/<id> survives gc and is out of refs/heads so nothing treats
                    # it as live work. Deletion without it is the loss this block prevents.
                    if git -C "$repo" update-ref "refs/slain/$ID" "$br" 2>/dev/null; then
                        parked="refs/slain/$ID"
                        say "work: $br carries work $base does not — parked at $parked"
                    else
                        say "work: could not park $br at refs/slain/$ID — REFUSING to delete it"
                        fail=1
                    fi
                fi
            fi
        elif [ "$MODE" = reopen ] && [ -n "$tip" ]; then
            # Cannot determine ahead count — default to keeping rather than deleting.
            kept_msg="branch $br kept in refs/heads at $tip (cannot check ahead count)"
            say "work: $kept_msg"
            branch_kept=1
        fi
    fi
    if [ "$branch_kept" = 0 ] && [ -n "$tip" ] && [ "$fail" = 0 ]; then
        if spira_destroy_branch "$ID" "$br" "$repo" "slain: $WHY" slain; then
            nuked="branch $br deleted at $tip${parked:+, kept at $parked}"; say "work: $nuked"
        else
            say "work: could not delete $br${SPIRA_DESTROY_ERR:+ — $SPIRA_DESTROY_ERR}"; fail=1
        fi
    fi
else
    say "work: bead names no resolvable repository — nothing to remove"
fi

# ---- 4b. the rest of the bead ------------------------------------------------------------
if [ -n "$nuked" ]; then bdq label remove "$ID" "branch:$br" >/dev/null 2>&1 || true; fi
note="Slain by the operator: $WHY. Aeon ${name:-?}${pid:+ (pid $pid)} stopped${unit:+ via $unit}. ${nuked:-${kept_msg:-work kept}}${saved:+; uncommitted changes salvaged to $saved}. No attempt charged."
st="$(status_of)"
case "$MODE" in
    close)  # MARK THE DROP BEFORE CLOSING. An operator close means "this work is not going to
            # happen", so no commit will ever name this bead — which is exactly the shape
            # sentinel CHECK 5 reopens. Without this label the close is undone within two
            # minutes and the bead returns as open work nobody will claim (sp-m56w, 00:18:52
            # on 2026-09-08). Set it first: a close that sticks matters more than the label.
            bdq label add "$ID" spira-dropped >/dev/null 2>&1 || say "bead: could not mark spira-dropped — the close may be reopened by CHECK 5"
            if [ "$st" = closed ]; then
                # Already closed — by the aeon before it was stopped, or by hand. The reason
                # still belongs on the record; re-closing would fail and say nothing. The note
                # is best-effort: bd refusing it (closed bead, permission, network) must not
                # flip the exit to non-zero when the bead state is already correct.
                bdq note "$ID" "$REASON — $note" >/dev/null 2>&1 || true
            else
                bdq close "$ID" --reason "$REASON — $note" >/dev/null 2>&1 || { say "bead: close failed"; fail=1; }
            fi ;;
    # THROUGH bead_reopen, LIKE EVERY OTHER REOPEN. The assignee is already cleared above,
    # where the lease is released — so this path was correct, but correct at a distance: its
    # correctness rested on a line sixty above it whose purpose is something else entirely,
    # and a later edit to either has no way to see the other. `bd reopen` keeps the assignee
    # and `bd ready --claim` skips an assigned bead while `bd ready` still lists it, which
    # makes a missed clearing invisible by construction — the bead really does go back to
    # open, and only the claim that never comes says otherwise. The helper is where that is
    # remembered; a slain aeon's name is the one that must not survive a reopen.
    reopen) bead_reopen "$ID" "$note" ;;
esac
bdjson show "$ID" | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; b=d[0] if d else {}
print("bead: %s status=%s assignee=%s labels=%s" % (b.get("id"), b.get("status"), b.get("assignee") or "-", ",".join(b.get("labels") or [])))' 2>/dev/null

# ---- 6. verify -------------------------------------------------------------------------
[ -z "$pid" ] || [ ! -d "/proc/$pid" ] || fail=1
ls "$SPIRA_RUN"/aeon-*-"$ID".pid >/dev/null 2>&1 && { say "verify: a pid file for $ID remains"; fail=1; }
[ "$KEEP" = 1 ] || [ -z "$repo" ] || [ "$branch_kept" = 1 ] || ! git -C "$repo" show-ref --verify -q "refs/heads/$br" || { say "verify: $br still exists"; fail=1; }
[ "$fail" = 0 ] && say "slain: $ID" || say "slay: INCOMPLETE for $ID — see above"
exit "$fail"
