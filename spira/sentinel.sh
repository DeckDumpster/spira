#!/usr/bin/env bash
#
# sentinel.sh — the outer harness: compare current state to goal state, close the gap.
#
#   sentinel.sh          one pass (this is what the timer runs)
#   sentinel.sh --report print the gap, change nothing
#
# THE SHAPE (the operator's call)
# --------------------------------
# "look at the current state, the goal state, reflect on the gap between those states, and
# then take necessary actions to close that gap ... cheap, quick, and frequent to run with
# deterministic heuristics; drop down to inference when judgement is required."
#
# So every check below is a deterministic predicate over beads and the commit graph, and
# each names the single action that closes its gap. Inference is reached only by the LAST
# check, and only when the deterministic ones have all passed and the DAG is nevertheless
# not moving — which is precisely the case where there is no rule to apply, because if
# there were a rule it would already be one of the checks above.
#
# Adding a check is how this system learns: a stall we diagnose by hand once becomes a
# deterministic check, and inference stops being asked about it. Inference is a cost
# centre, not a feature.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

REPORT=0; [ "${1:-}" = "--report" ] && REPORT=1
POISON_AT="${SPIRA_POISON_AT:-3}"
REQUEUE_AT="${SPIRA_REQUEUE_AT:-5}"
RECLAIM_AT="${SPIRA_RECLAIM_AT:-5}"
# THERE IS NO $REPO HERE ANY MORE, and that is the point of this bead. The repositories
# this harness lands into are plural and come from repo-map, because a bead now names its
# own through a `repo:` label; a single module-level $REPO is exactly the constant that
# made Spira able to work one repository out of seven. `spira_repos` enumerates them and
# every check that touches a commit graph asks which one it is standing in. SPIRA_REPO
# still names the HOME repository, so the suite can drive the whole harness against a
# fixture with a real origin — the landing path was once the one part no test could reach.
# Every persona in the chamber, not a hardcoded name. A default of `builder` meant a fayth
# could land complete and never once be evaluated — ops.fayth shipped inert that way on
#. SPIRA_FAYTHS still overrides, because which personas a HOST runs is deployment
# configuration; what it must not be is the only thing that makes a persona exist.
FAYTHS="$(spira_fayths)"
# THE PARTITIONS THIS PASS SWEEPS, read from the chamber once. Every check below that asks
# the database a question about "the work" asks it once per partition: naming one of them —
# `spira,plan`, the builder's — is how the Sending, stalled-work reporting and landing
# verification came to watch a single persona while reading as if they watched the harness.
PARTITIONS="$(fayth_partitions)"
COOLDOWN="$SPIRA_RUN/inference.cooldown"
INFERENCE_EVERY="${SPIRA_INFERENCE_EVERY:-3600}"   # seconds; judgement is expensive

# TWO COUNTERS, BECAUSE THEY ANSWER DIFFERENT QUESTIONS.
# `acted`      — a write happened. Used only for the pass summary.
# `progressed` — the DAG actually MOVED: a bead changed status, or a branch landed.
#
# CHECK 8 gates on `progressed` and must NEVER gate on `acted`. Gating judgement on "did
# anything write" makes every false or futile action a mute button on the one check that
# notices paralysis — and the failure is not hypothetical or rare, it is structural:
# CHECK 3's precondition (0 ready, 0 in progress, work open) IS CHECK 8's precondition,
# and `bd recompute-blocked` exits 0 whether or not it changed a flag. So a starved
# harness wrote, counted an action, and silenced its own judgement tier on EVERY pass.
# Measured: 76 passes, judgement fired zero times (SP_SINCE_JUDGEMENT='>76').
acted=0        # a write happened
progressed=0   # the DAG moved
act()      { acted=$((acted+1)); log "ACT $*"; }
progress() { progressed=$((progressed+1)); act "$@"; }

# ======================================================================================
# DATABASE CHECK. Verify bd can reach $SPIRA_DB before reading any state. When bd
# cannot reach the database, every state read — goal_open_children, plan_ready,
# plan_inprog — returns 0 or empty, so GOAL_REACHED fires on the same evidence that
# a working, goal-complete sentinel and a broken one produce: six minutes of DB outage
# read as "pass complete — 0 action(s), 0 progress, goal reached" (sp-4fss).
#
# This also ensures a zero-capacity pass (SPIRA_MAX_AEONS=0, used by the test instance
# permanently) is distinguishable in the log from a pass that could not read the graph:
# both produce zero actions and zero progress, but this check makes one exit 1 with
# "DATABASE UNREADABLE" while the other reaches "goal reached" on confirmed state.
#
# bdq is used rather than bdjson because bdjson pipes through sed (json_only) and
# always exits 0 regardless of whether bd itself succeeded; the underlying bd call is
# what says whether the database is reachable.
#
# SPIRA_SKIP_RECLAIM=1: skip the db check. In a test fixture the database is always
# reachable; the check costs ~500ms per pass and the 16 passes in suites that cover
# the poison valve exhaust the per-suite budget before CHECK 4 is exercised.
# ======================================================================================
if [ "${SPIRA_SKIP_RECLAIM:-0}" != 1 ]; then
if ! bdq list --limit 1 >/dev/null 2>&1; then
    log "DATABASE UNREADABLE — bd cannot reach $SPIRA_DB; state is unknown and this pass cannot close any gap"
    exit 1
fi
fi

# ======================================================================================
# GOAL BEAD CHECK. A SPIRA_GOAL that names no bead makes the completion test vacuously
# true: goal_open_children returns nothing, n_open=0, and every pass announces "goal
# reached" against a dangling reference (sp-ejf3). Refuse to proceed when the goal
# cannot be read so the signal cannot fire against nothing.
#
# Inside the same SPIRA_SKIP_RECLAIM guard as the state queries: fixtures that set it
# also skip the completion path, so they do not need a goal bead in the fixture store.
# ======================================================================================
if [ "${SPIRA_SKIP_RECLAIM:-0}" != 1 ]; then
if [ -z "$(bdjson show "$SPIRA_GOAL" | head -c 1)" ]; then
    log "GOAL UNRESOLVABLE — $SPIRA_GOAL names no bead in $SPIRA_DB; this pass cannot assess completion"
    exit 1
fi
fi

# ======================================================================================
# STATE
# ======================================================================================
# SPIRA_SKIP_RECLAIM=1: skip goal_open_children, plan_ready, and plan_inprog. These
# three queries together cost ~1.5s per pass (~24s across 16 passes). The downstream
# consumers in CHECK 3 and CHECK 8 are either already guarded by SPIRA_SKIP_RECLAIM or
# are not asserted on by fixtures that set it; treating them as zero is correct for test
# purposes.
if [ "${SPIRA_SKIP_RECLAIM:-0}" != 1 ]; then
    open_children="$(goal_open_children)"
    n_open="$(printf '%s' "$open_children" | grep -c . || true)"
    # THE PLAN'S numbers, and they are named that way deliberately. These two feed CHECK 3
    # and CHECK 8, both of which reason about the DAG under $SPIRA_GOAL, so the plan
    # predicate is the right one for them and the WRONG one for anything else. Reading an
    # unqualified `ready` as "is there work" is what made CHECK 7 gate every persona on
    # the builder's partition. Whether a persona has work is fayth_ready, in CHECK 7.
    plan_ready="$(ready_count "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" "spira-poison,$SPIRA_ASK_LABEL")"
    plan_inprog="$(bdjson list --status in_progress --limit 0 --label "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" | json_count)"
else
    open_children=""; n_open=0; plan_ready=0; plan_inprog=0
fi
live=0; for f in $FAYTHS; do live=$((live + $(aeon_count "$f"))); done

log "state: goal=$SPIRA_GOAL open=$n_open plan_ready=$plan_ready in_progress=$plan_inprog aeons=$live fayths=[$FAYTHS]"

# A NARROWED ROSTER SAYS SO (roster_warnings, in lib.sh).
roster_warnings "$FAYTHS"

if [ "$REPORT" = 1 ]; then
    printf '\nOpen beads under %s:\n' "$SPIRA_GOAL"
    printf '%s\n' "$open_children" | sed 's/^/  /'
    exit 0
fi

# ======================================================================================
# CHECK 1 — completed pilgrimages. An epic whose children have all closed is done: announce
# it to its subscribers, then close it. This is `gt convoy check` plus `gt convoy watch`
# rebuilt on epic beads; pilgrimage.sh holds the detection, the watcher list and the manual
# entry points (`pilgrimage.sh list|watch|unwatch`).
#
# The announcement the operator sees is an EVENT — a closed `event` bead of kind
# `pilgrimage.complete`, emitted through `ask.sh note`. Machinery writes outcomes, never
# insights: an insight is what an agent LEARNED and might become law, and mixing outcomes
# into that queue is how the one bin that survived a refresh filled with completion notices.
#
# Generalised past $SPIRA_GOAL deliberately. The goal epic is only one pilgrimage, and a
# harness that can announce exactly one of them announces nothing the moment a second
# design is in flight — which is the state Gas Town was in with six convoys open.
# ======================================================================================
completed="$("$SPIRA_HOME/pilgrimage.sh" check 2>&1)"
[ -n "$completed" ] && printf '%s\n' "$completed"
n_done="$(printf '%s' "$completed" | grep -c '^PILGRIMAGE COMPLETE' || true)"
[ "${n_done:-0}" -gt 0 ] && progress "announced and closed $n_done completed pilgrimage(s)"

GOAL_REACHED=0
if [ "$n_open" -eq 0 ]; then
    # NOT an early exit. The last bead of a pilgrimage is the one whose branch is most
    # likely to be sitting unlanded and uncleaned, because it closes in the same pass that
    # would have tidied it — so returning here is how a finished pilgrimage leaves a branch,
    # a worktree, and possibly a closed-but-unlanded bead behind it forever. Everything up
    # to CHECK 6b still applies with nothing open, and so does SUMMONING: whether a persona
    # has work is its own predicate's answer, not the plan's. Ops receives production events
    # from outside the plan entirely, so exiting here because a design finished is exactly
    # the "unreachable by construction" defect. Only JUDGEMENT does not apply, and CHECK 8
    # already declines to run with nothing open.
    log "goal reached — $SPIRA_GOAL has no open children; finishing the sending"
    GOAL_REACHED=1
fi

# ======================================================================================
# CHECK 2 — dead workers. A lease outlives the aeon that took it; reclaim is the reaper.
# Grace window ~2x the TTL so a briefly paused worker is not robbed of live work.
#
# SPIRA_SKIP_RECLAIM=1 bypasses this check and CHECK 2c. Each bdq reclaim call costs
# ~600ms and a fixture that creates no stale leases pays that on every pass with nothing
# to show for it. Test suites that cover CHECK 4 (the poison valve) rather than reclaim
# behaviour set this to halve the per-pass wall time (16 passes × ~2s saved = ~32s).
#
# ONE RECLAIM PER PARTITION, ASKED THROUGH THE CHAMBER. This named `spira,plan` — the
# builder's partition standing in for every persona — so an ops or spike aeon that died left
# its bead in_progress with a dead lease and no time-based reaper ever looked at it. The
# /proc ghost sweep in CHECK 2b catches that case faster in practice, but the backstop for
# everything /proc cannot see did not exist for those partitions at all.
#
# The loop is check2_reclaim_stale (lib.sh), which matches the SUCCESS shape via
# parse_reclaimed rather than the word "reclaim" (which also appears in the idle message).
if [ "${SPIRA_SKIP_RECLAIM:-0}" != 1 ]; then
check2_protect_waiting
check2_reclaim_stale "$PARTITIONS"
fi

# ======================================================================================
# CHECK 2b — stranded work. This is `gt convoy stranded` rebuilt, and it sits here because
# its ghost case is a faster, evidence-based version of CHECK 2: reclaim above is a time
# heuristic about liveness it cannot observe, while strand.sh asks /proc whether the aeon
# that took the lease still exists. A bead in_progress with a dead holder is reclaimed in
# minutes rather than at the three-hour grace window.
#
# It acts where the fix is mechanical and escalates ONCE where it is not, so it neither
# retries forever nor pages the operator every two minutes. `strand.sh report` is the human view.
# ======================================================================================
stranded="$("$SPIRA_HOME/strand.sh" check 2>&1)"
[ -n "$stranded" ] && printf '%s\n' "$stranded"
n_strand="$(grep -cE '^(RECLAIMED|RECOMPUTED|STRANDED)' <<< "$stranded" || true)"
n_moved="$(grep -cE '^RECLAIMED' <<< "$stranded" || true)"
n_escal="$(grep -cE '^STRANDED' <<< "$stranded" || true)"
# A reclaim moved the DAG; an escalation only wrote. A standing escalation that never
# clears would otherwise count as an action on every pass and mute judgement forever.
[ "${n_moved:-0}" -gt 0 ] && progress "handled $n_moved stranded item(s)"
[ "${n_escal:-0}" -gt 0 ] && act "escalated $n_escal stranded item(s)"
# The counts above are deliberately not recomputed. A reclaimed ghost becomes ready, and the
# next pass — two minutes away — summons for it; re-querying here would duplicate three
# queries to save that.

# ======================================================================================
# CHECK 2c — orphaned claims. The third dead-worker case, and the one neither check above
# can see: a bead that is OPEN, carries an assignee, and holds no lease.
#
# CHECK 2 reverts stale-lease in_progress issues and CHECK 2b witnesses a dead holder of
# one. Both are about a lease. This is about a bead whose STATUS was already reset — by a
# reopen in the landing pass, in CHECK 5, or in the aeon's own closed-without-a-commit
# check — while its assignee was left standing. `bd ready` counts such a bead and `bd ready
# --claim` skips it, so it is ready forever and claimable never, and no lease ever expires
# to rescue it. Thirteen plan beads were stuck this way on 2026-09-06 while CHECK 7 summoned
# an aeon every two minutes to report idle within one second.
#
# bead_reopen and release_own_claim now write the two facts together, so this should find
# nothing. It stays because it is the POSITIVE CONTROL on that claim: a path that ends a
# claim without clearing it is a bug that presents as a healthy queue, and this sweep is
# what turns that silence into a visible RELEASED line
# (law-absence-needs-a-positive-control).
#
# Skipped when SPIRA_SKIP_RECLAIM=1 — see CHECK 2 above.
#
# ONE SWEEP PER PARTITION, ASKED THROUGH THE CHAMBER — see release_orphan_claims_partitions
# (lib.sh). A single hardcoded `${SPIRA_SCOPE_LABEL},plan` call left orphaned claims in ops,
# spike or groom partitions unreleased forever: nothing else in the harness sweeps them.
# ======================================================================================
if [ "${SPIRA_SKIP_RECLAIM:-0}" != 1 ]; then
released="$(release_orphan_claims_partitions "$PARTITIONS")"
[ -n "$released" ] && printf '%s\n' "$released"
n_rel="$(grep -c '^RELEASED' <<< "$released" || true)"
if [ "${n_rel:-0}" -gt 0 ]; then
    progress "released $n_rel orphaned claim(s)"
    # Every bead the sweep freed is claimable NOW, so the count CHECK 3 and CHECK 8 reason
    # about is stale by exactly this much. Re-queried only when something actually moved.
    plan_ready="$(ready_count "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" "spira-poison,$SPIRA_ASK_LABEL")"
fi
fi

# ======================================================================================
# CHECK 3 — stale blocked flags. is_blocked is a cached column and goes wrong after an
# import or a pull; a whole DAG can sit "blocked" behind dependencies that all closed.
#
# Skipped when SPIRA_SKIP_RECLAIM=1: a fixture that seeds all beads explicitly has a
# correct is_blocked column by construction, so recompute-blocked finds nothing and costs
# two bd calls (~700ms) for no gain.
# ======================================================================================
if [ "${SPIRA_SKIP_RECLAIM:-0}" != 1 ] \
    && [ "$plan_ready" -eq 0 ] && [ "$plan_inprog" -eq 0 ] && [ "$n_open" -gt 0 ]; then
    bdq recompute-blocked >/dev/null 2>&1
    was="$plan_ready"
    plan_ready="$(ready_count "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" "spira-poison,$SPIRA_ASK_LABEL")"
    # LOG it always, COUNT it only when it changed something. recompute-blocked exits 0
    # either way, so trusting its exit status made this check fire on exactly the state
    # CHECK 8 exists to detect, and mute it — every pass, for 76 passes.
    log "recomputed is_blocked"
    [ "$plan_ready" != "$was" ] && progress "recompute-blocked freed $plan_ready bead(s)"
fi

# ======================================================================================
# CHECK 4 — poison. A bead that has failed N times is not retried again; retrying it is
# how one bad bead burns tokens forever. This is mountain's skip-after-N-failures, and it
# is the property most likely to be lost silently, because nothing complains when it is
# missing.
#
# IT ITERATES WHAT THE SUMMONER CAN DISPATCH, never the goal epic's children. Those are two
# different sets and the gap between them is unpoisonable work: a bead carrying a partition's
# labels but parented outside $SPIRA_GOAL was summoned every pass, failed every time, and
# never reached the valve. dispatchable_open carries the whole argument.
#
# THE POISONED BEAD KEEPS ITS CLAIM. If a live aeon holds it, unclaiming here would cut the
# lease out from under a session that is still writing — and the aeon releases on its own
# exit path anyway. Labelling is sufficient: every fayth's partition excludes spira-poison,
# so the moment the holder lets go, CHECK 7 stops summoning for it.
# ======================================================================================
dispatchable="$(dispatchable_open)"
log "CHECK4 examining $(printf '%s' "$dispatchable" | grep -c . || true) dispatchable bead(s), poison=$POISON_AT requeue=$REQUEUE_AT reclaim=$RECLAIM_AT"
# ONE QUERY FOR ALL ATTEMPTS, REOPENS AND RECLAIMS. dispatchable_open already parsed each
# bead's labels and emits them alongside the id; CHECK 4 reads them from there, not the
# store. attempts_of/reopens_of/reclaims_of are per-bead SQL calls; one GROUP BY returns
# the same numbers for the whole dispatchable set. On a 59-bead set: 177 calls / 29,731 ms
# → 1 call / 178 ms.
declare -A _c4_attempts _c4_reopens _c4_reclaims
while IFS=$'\t' read -r _bid _batt _brep _brcl; do
    [ -n "$_bid" ] || continue
    _c4_attempts["$_bid"]="${_batt:-0}"
    _c4_reopens["$_bid"]="${_brep:-0}"
    _c4_reclaims["$_bid"]="${_brcl:-0}"
done < <(check4_bulk_data "$dispatchable")
while IFS=$'\t' read -r id _labels; do
    [ -n "$id" ] || continue
    # ATTEMPTS, REQUEUES AND RECLAIMS FROM THE EVENTS TRAIL; labels for poison, repo, and
    # partition exclusions. Counter labels (sp-attempt-N, sp-reclaim-N, sp-requeue-N) are
    # no longer written (sp-lzt) — every count here comes from the bulk pre-load above.
    n="${_c4_attempts[$id]:-0}"; n="${n:-0}"
    _reclaims="${_c4_reclaims[$id]:-0}"; _reclaims="${_reclaims:-0}"
    _requeues="${_c4_reopens[$id]:-0}"; _requeues="${_requeues:-0}"

    # THE DEDUP STATE IS I/O (a file read); check4_decide itself is not — see lib.sh. Read
    # it once per bead and hand it to the pure decision as a bundled stamp.
    _rq_asked=0; requeue_asked "$id" && _rq_asked=1
    _rc_asked=0; reclaim_asked "$id" && _rc_asked=1
    _po_asked=0; poison_asked "$id" "$n" && _po_asked=1
    decision="$(check4_decide "$n" "$_requeues" "$_reclaims" "$_labels" "$_rq_asked:$_rc_asked:$_po_asked")"

    # REQUEUE CAP. A bead completed and requeued past the cap is stuck in a loop the harness
    # is causing: the session finished the work, closed the bead, and the harness put it back
    # each time because the branch could not rebase onto a base that had moved. The work may
    # be correct; the queue cannot get it to land. Distinct from poison and reclaim: check4_decide
    # decides each of the three independently, so a bead over one cap is never silently
    # exempted from the others.
    case " $decision " in *' requeue-mail '*)
        _rq_causes="$(printf '%s' "$_labels" | tr ',' '\n' \
            | grep -E '^sp-requeue-[0-9]+(-|$)' \
            | sed -E 's/^sp-requeue-([0-9]+)$/\1 unrecorded/;s/^sp-requeue-([0-9]+)-(.*)$/\1 \2/' \
            | sort -n | awk '{printf "%s%s x%s", sep, $2, $1; sep=", "} END{printf "\n"}')" || true
        _rq_causes="${_rq_causes:-unrecorded}"
        _rq_subj="Spira bead $id — completed and requeued $_requeues times, never landed (${_rq_causes}) — the harness cannot land it"
        _rq_dflt="close the bead if its work has already landed under a different id or is no longer needed; file a harness-defect bead if the deliverable was not a commit; otherwise label it needs-rebase so an aeon can resolve the conflict"
        _rq_ev="$(bead_context "$id" 2>/dev/null || printf '(could not read %s)' "$id")

REQUEUES  $_requeues (cap $REQUEUE_AT) — causes: ${_rq_causes}
ATTEMPTS  $n — distinct from requeues; a requeue is not a failed attempt and was not charged"
        if [ -x "$SPIRA_HOME/mail.sh" ] && SPIRA_MAIL_REPEAT_CONSIDERED="sentinel-own-dedup" \
              "$SPIRA_HOME/mail.sh" send operator \
              --from "Sentinel <sentinel@spira>" \
              --subject "$_rq_subj" \
              --kind question \
              --default "$_rq_dflt" <<MAILEOF >/dev/null 2>&1; then
## Question
$_rq_subj

## Default
$_rq_dflt

$_rq_ev

Every additional requeue costs one full aeon session and its context budget; no progress is made toward landing this work.
MAILEOF
            requeue_asked_mark "$id" "$_requeues"
        else
            log "CHECK4 $id: requeue escalation path refused the ask — retries next pass"
        fi
        ;;
    esac

    # RECLAIM CAP. A bead N aeons have died holding is on a box that cannot run it. The
    # sessions never judged the work; the infrastructure killed them. Different from a cycling
    # requeue: the issue is the box, not the queue. No poison label is added — the work is not
    # at fault.
    case " $decision " in *' reclaim-mail '*)
        _rc_causes="$(printf '%s' "$_labels" | tr ',' '\n' \
            | grep -E '^sp-reclaim-[0-9]+(-|$)' \
            | sed -E 's/^sp-reclaim-([0-9]+)$/\1 unrecorded/;s/^sp-reclaim-([0-9]+)-(.*)$/\1 \2/' \
            | sort -n | awk '{printf "%s%s x%s", sep, $2, $1; sep=", "} END{printf "\n"}')" || true
        _rc_causes="${_rc_causes:-unrecorded}"
        _rc_subj="Spira bead $id — $_reclaims aeons died holding it, work never judged (${_rc_causes}) — the box cannot run it"
        _rc_dflt="close the bead if the work is no longer relevant; move it to a healthier lane if this box consistently kills workers; otherwise check infrastructure and re-queue when the box is stable"
        _rc_ev="$(bead_context "$id" 2>/dev/null || printf '(could not read %s)' "$id")

RECLAIMS  $_reclaims (cap $RECLAIM_AT) — causes: ${_rc_causes}
ATTEMPTS  $n — distinct from reclaims; no attempt was ever charged"
        if [ -x "$SPIRA_HOME/mail.sh" ] && SPIRA_MAIL_REPEAT_CONSIDERED="sentinel-own-dedup" \
              "$SPIRA_HOME/mail.sh" send operator \
              --from "Sentinel <sentinel@spira>" \
              --subject "$_rc_subj" \
              --kind question \
              --default "$_rc_dflt" <<MAILEOF >/dev/null 2>&1; then
## Question
$_rc_subj

## Default
$_rc_dflt

$_rc_ev

The workers above were killed by the infrastructure before they could act. This is a fact about the box, not the work.
MAILEOF
            reclaim_asked_mark "$id" "$_reclaims"
        else
            log "CHECK4 $id: reclaim escalation path refused the ask — retries next pass"
        fi
        ;;
    esac

    # POISON AND ASK SHARE ONE GATE: a CLOSED bead never poisons and never asks.
    # dispatchable_open excludes closed beads, but it is a SNAPSHOT and this loop makes
    # several bd calls per bead — so a bead the landing pass finished a few seconds ago is
    # still in the list, and the operator was asked whether to change the approach on work
    # that had already landed. Re-read the one field that decides it, immediately before
    # acting on it — and only when check4_decide found something to act on.
    case " $decision " in *' poison '*|*' ask '*)
        # ONE bdjson show IS ALSO USED FOR bead_context. When the ask fires (the first time
        # at this count), the evidence block needs the full bead data anyway. Fetch it once
        # and pass the JSON to both the status check and the context formatter rather than
        # making a second identical bd call for the context alone.
        _bd_json="$(bdjson show "$id" 2>/dev/null)" || _bd_json=""
        _bead_st="$(printf '%s' "$_bd_json" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print(""); sys.exit()
d = d if isinstance(d, list) else [d]
print(d[0].get("status", "") if d else "")' 2>/dev/null)"
        if [ "$_bead_st" = closed ]; then
            log "CHECK4 $id: $n attempts, but it closed while this pass ran — not poisoned, not asked"
        else
            # THE POISON NAMES THE OUTCOMES THAT CHARGED IT, never just their count. Attempts
            # are counted from the events trail (sp-lzt); the per-cause breakdown labels once
            # provided is gone, the count itself is the signal.
            #
            # Check the poison label from _labels. `... | grep -q` under `set -o pipefail`
            # hands back 141 when it MATCHES — grep exits at the first hit and the writer
            # dies of SIGPIPE — so `if ! ... | grep -q spira-poison` read as "not poisoned"
            # precisely when the bead was (law-no-grep-q-under-pipefail). check4_decide's
            # case match on the already-fetched label string has neither hazard.
            case " $decision " in *' poison '*)
                bdq label add "$id" spira-poison >/dev/null 2>&1
                bdq note "$id" "Poisoned after $n in_progress transition(s) without landing. Not retried until a human changes the approach. Any live holder keeps its claim and releases on its own exit path; no persona can claim it again while the label stands." >/dev/null 2>&1
                progress "poisoned $id after $n attempts"
                # check4_decide only emits `poison` on the transition into poisoned (the
                # `spira-poison` case in its label match), so this fires once — the bead
                # keeps the label, and every later pass takes the other branch.
                spira_event bead.poisoned "$id" "poisoned $id after $n attempts" \
                    "not retried until a human changes the approach" || true
                ;;
            esac

            # AT MOST ONE ASK PER (BEAD, ATTEMPT COUNT), EVER — check4_decide applied that
            # dedup via the asked-stamp above; poison_asked_mark below is the write half.
            # The ask's own remedy is to clear the poison label, so keying dedup on the
            # label (rather than the count) made every application of the remedy re-arm the
            # ask (poison_asked, lib.sh).
            case " $decision " in *' ask '*)
                # The ask carries the failure itself. A path is not evidence: the operator
                # reads this in a tmux pane and cannot open a file from it. THE BEAD FIRST,
                # THEN THE FAILURE — a log tail says what broke, not what the work was for.
                # The JSON was already fetched for the status check above — pipe it to the
                # context formatter rather than calling bead_context (which would re-issue
                # bdjson show).
                ev="$(printf '%s' "$_bd_json" | python3 -c '
import sys, json, datetime
try:
    d = json.load(sys.stdin)
    i = (d if isinstance(d, list) else [d])[0]
except Exception:
    print("(could not read the bead — say so rather than pretend)"); raise SystemExit
def age(ts):
    try:
        t = datetime.datetime.fromisoformat(str(ts).replace("Z", "+00:00"))
        h = (datetime.datetime.now(datetime.timezone.utc) - t).total_seconds() / 3600
        return "%dh" % h if h < 48 else "%dd" % (h / 24)
    except Exception:
        return "?"
print("BEAD    %s  [%s, P%s, open %s]" % (i.get("id"), i.get("status"), i.get("priority"), age(i.get("created_at"))))
print("TITLE   %s" % (i.get("title") or "(none)"))
labs = ", ".join(i.get("labels") or []) or "(none)"
print("LABELS  %s" % labs)
print("")
print("WHAT THIS BEAD IS FOR")
print((i.get("description") or "(no description — that is itself the problem)").strip())
notes = i.get("notes")
if isinstance(notes, str):
    notes = [n for n in notes.split("\n") if n.strip()]
elif isinstance(notes, list):
    notes = [(n.get("text") if isinstance(n, dict) else str(n)) for n in notes]
else:
    notes = []
if notes:
    print("")
    print("MOST RECENT NOTES")
    for n in notes[-3:]:
        print("  - %s" % str(n).strip()[:400])
' 2>/dev/null || printf '(could not read %s)' "$id")"
                # THE BRANCH IS LOOKED FOR IN THE BEAD'S OWN REPOSITORY. Asking the home
                # repo about another repository's bead answers "none — nothing was
                # committed" for work that is sitting on a branch in another checkout, and
                # the operator would be deciding whether to drop a bead on the strength of a
                # fact from the wrong disk. r_name comes from the labels dispatchable_open
                # emitted for this bead.
                r_name="$(printf '%s' "$_labels" | tr ',' '\n' | sed -n 's/^repo://p' | head -1)"
                r_name="${r_name:-$(spira_home_repo)}"
                r_path="$(repo_root "$r_name")" || r_path=""
                # COMMIT COUNT AND DIFFSTAT, NOT REF EXISTENCE. show-ref returns true for a
                # branch that exists but has zero commits ahead of base — reporting "with
                # work on it" when none exists sends the operator looking for output that
                # was never written (sp-njwb).
                branch_info='none — nothing was committed'
                if [ -n "$r_path" ] && git -C "$r_path" show-ref --verify -q "refs/heads/spira/$id" 2>/dev/null; then
                    _base="$(spira_landref "$r_path" 2>/dev/null)" || _base=""
                    _range="${_base:+${_base}..}spira/$id"
                    _nc="$(git -C "$r_path" rev-list --count "$_range" 2>/dev/null)" || _nc="?"
                    if [ "${_nc}" = 0 ] || [ "${_nc}" = "?" ]; then
                        branch_info="spira/$id exists, no commits${_base:+ ahead of $_base}"
                    else
                        _ds="$(git -C "$r_path" diff --stat "$_range" 2>/dev/null | tail -1)"
                        branch_info="spira/$id — ${_nc} commit(s)${_ds:+; $_ds}"
                    fi
                fi
                # Attempts are from the events trail (sp-lzt): no per-cause breakdown.
                charge_summary="$n in_progress transition(s)"
                ev="$ev

REPO      $r_name${r_path:+ ($r_path)}
ATTEMPTS  $n (poison threshold $POISON_AT) — each in_progress transition from the events trail
BRANCH    $branch_info

--- last session log (tail) ---
$(trace_tail "$SPIRA_RUN/$id.log" 25)"
                # MARKED ONLY IF THE MAIL WAS ACCEPTED. Stamping first would let mail.sh
                # being absent silently swallow the one notification this count produces.
                _po_subj="Spira bead $id — ${charge_summary} without landing (${n} attempts) — change the approach or drop it?"
                _po_dflt="if the work is correct, re-label or split the bead and clear spira-poison; if it is not worth doing, close it"
                if [ -x "$SPIRA_HOME/mail.sh" ] && "$SPIRA_HOME/mail.sh" send operator \
                      --from "Sentinel <sentinel@spira>" \
                      --subject "$_po_subj" \
                      --kind question \
                      --default "$_po_dflt" <<MAILEOF >/dev/null 2>&1; then
## Question
$_po_subj

## Default
$_po_dflt

nothing downstream of it can proceed, and no aeon will take it again while it is poisoned

$ev
MAILEOF
                    poison_asked_mark "$id" "$n"
                else
                    log "CHECK4 $id: the escalation path refused the ask — it stands, and the next pass retries it"
                fi
                ;;
            esac
        fi
        ;;
    esac
done <<< "$dispatchable"

# CHECK 4 SUPPLEMENT — requeue cap for closed-but-unlanded beads.
# dispatchable_open drops every closed bead, so a bead whose cycle ends closed-and-unlanded
# is never examined by the loop above. Find closed beads that carry a branch: label, get
# their reopen counts, and apply the same escalation for those at or above the cap that
# have not yet landed. requeue_asked/requeue_asked_mark provide the same per-bead dedup.
_c4_closed="$(check4_closed_branched)"
if [ -n "$_c4_closed" ]; then
    declare -A _c4c_reopens
    while IFS=$'\t' read -r _bid _batt _brep _brcl; do
        [ -n "$_bid" ] || continue
        _c4c_reopens["$_bid"]="${_brep:-0}"
    done < <(check4_bulk_data "$_c4_closed")
    while IFS=$'\t' read -r id _labels; do
        [ -n "$id" ] || continue
        _requeues="${_c4c_reopens[$id]:-0}"; _requeues="${_requeues:-0}"
        # attempts and reclaims do not apply to a closed bead's requeue-mail decision; the
        # reclaim/poison asked-flags are pinned true so check4_decide never emits
        # poison/clear/ask/reclaim-mail here — this call site only ever wants requeue-mail.
        _rq_asked=0; requeue_asked "$id" && _rq_asked=1
        decision="$(check4_decide 0 "$_requeues" 0 "$_labels" "$_rq_asked:1:1")"
        case " $decision " in *' requeue-mail '*) ;; *) continue ;; esac
        _r_name="$(printf '%s' "$_labels" | tr ',' '\n' | sed -n 's/^repo://p' | head -1)"
        _r_name="${_r_name:-$(spira_home_repo)}"
        _r_path="$(repo_root "$_r_name" 2>/dev/null)" || _r_path=""
        if [ -n "$_r_path" ] && landed "$id" "$_r_path"; then
            log "CHECK4-closed $id: reopens=$_requeues but already landed — no escalation"
            continue
        fi
        _rq_causes="$(printf '%s' "$_labels" | tr ',' '\n' \
            | grep -E '^sp-requeue-[0-9]+(-|$)' \
            | sed -E 's/^sp-requeue-([0-9]+)$/\1 unrecorded/;s/^sp-requeue-([0-9]+)-(.*)$/\1 \2/' \
            | sort -n | awk '{printf "%s%s x%s", sep, $2, $1; sep=", "} END{printf "\n"}')" || true
        _rq_causes="${_rq_causes:-unrecorded}"
        _rq_subj="Spira bead $id — completed and requeued $_requeues times, never landed (${_rq_causes}) — the harness cannot land it"
        _rq_dflt="close the bead if its work has already landed under a different id or is no longer needed; file a harness-defect bead if the deliverable was not a commit; otherwise label it needs-rebase so an aeon can resolve the conflict"
        _rq_ev="$(bead_context "$id" 2>/dev/null || printf '(could not read %s)' "$id")

REQUEUES  $_requeues (cap $REQUEUE_AT) — causes: ${_rq_causes}
STATUS    closed (not landed in ${_r_name:-unknown})"
        if [ -x "$SPIRA_HOME/mail.sh" ] && SPIRA_MAIL_REPEAT_CONSIDERED="sentinel-own-dedup" \
              "$SPIRA_HOME/mail.sh" send operator \
              --from "Sentinel <sentinel@spira>" \
              --subject "$_rq_subj" \
              --kind question \
              --default "$_rq_dflt" <<MAILEOF >/dev/null 2>&1; then
## Question
$_rq_subj

## Default
$_rq_dflt

$_rq_ev

Every additional requeue costs one full aeon session and its context budget; no progress is made toward landing this work.
MAILEOF
            requeue_asked_mark "$id" "$_requeues"
        else
            log "CHECK4-closed $id: requeue escalation path refused the ask — retries next pass"
        fi
    done <<< "$_c4_closed"
fi

# STALE POISON CLEAR. spira-poison is added when a bead's attempt count reaches the
# threshold; the operator clears it after changing the approach. But the label also becomes
# stale when someone removes attempt labels and the count drops below the threshold: the
# label makes the bead invisible to dispatchable_open, so CHECK 4 never evaluates it, and
# nothing can clear it — a circular dependency that makes the hold permanent (defect sp-9szt).
#
# Scan all poisoned non-closed beads. Any whose count is now below the threshold is
# released: the condition that warranted the hold is gone.
# EVENTS-BASED COUNT: attempt count comes from status_changed events, not labels (sp-lzt).
# Each poisoned bead is queried individually; the per-bead cost is one SQL call. This scan
# iterates the (usually small) poisoned set, not the whole dispatchable set — the N+1
# query sp-f1m7f removed is the CHECK4 main loop above, not this one.
while read -r id; do
    [ -n "$id" ] || continue
    n="$(attempts_of "$id")"; n="${n:-0}"
    decision="$(check4_decide "$n" 0 0 "spira-poison" "1:1:1")"
    case " $decision " in *' clear '*) ;; *) continue ;; esac
    bdq label remove "$id" spira-poison >/dev/null 2>&1
    progress "CHECK4 $id: stale poison cleared — $n attempt(s), below threshold $POISON_AT"
done < <(bdjson list --limit 0 --label spira-poison 2>/dev/null \
    | python3 -c '
import json, sys
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("status") == "closed" or i.get("issue_type") in ("epic", "event"):
        continue
    print(i["id"])
' 2>/dev/null || true)

# ======================================================================================
# CHECK 5 — closed but not landed. A bead closed with no commit naming it unblocks its
# dependents on a lie, and everything downstream then builds on work that is not there.
#
# Skipped when SPIRA_SKIP_CLOSED_CHECK=1: a fixture that explicitly seeds all beads has no
# $SPIRA_RUN/<id>.log files, so the while loop's first guard (`[ -f $SPIRA_RUN/$id.log ]`)
# would skip every row anyway — but the two `bdjson list --status closed` queries per
# partition still each cost ~500ms with nothing to show. Skipping the whole block saves ~1s
# per sentinel pass in suites that do not test the closed-not-landed path.
#
# DELIBERATELY NOT tied to SPIRA_SKIP_RECLAIM. test-check5-drop.sh sets SPIRA_SKIP_RECLAIM=1
# to skip the expensive overhead (DB check, STATE queries, CHECK 2, CHECK 3) while still
# exercising this check. Suites that want to skip CHECK 5 must set SPIRA_SKIP_CLOSED_CHECK=1
# explicitly (test-poison.sh and test-requeue-cap.sh do this to keep their per-pass budget).
# ======================================================================================
if [ "${SPIRA_SKIP_CLOSED_CHECK:-0}" != 1 ]; then
# The repository comes out of the SAME query as the id. `landed` reads the commit graph, and
# reading the wrong repository's graph gives the wrong answer confidently in both directions:
# a bead for repository A reads as never landed in repository B, so this check would reopen finished
# work on every pass. One query, both facts.
# THE FIELD SEPARATOR IS \x1f, NOT TAB, AND THAT IS LOAD-BEARING. Bash treats tab as IFS
# WHITESPACE, so a run of tabs collapses to one delimiter and an EMPTY MIDDLE COLUMN
# disappears, shifting every column after it left by one. `delivers` is empty on almost every
# bead — it is the exception, not the rule — so `read` assigned it the NEXT column,
# started_at's ISO timestamp. A timestamp is not a recognised delivers type, so CHECK 5 took
# the delivers branch for every closed bead that had no delivers: label at all, charged an
# attempt, and reopened it. Eleven beads were poisoned in ninety seconds on 2026-09-09 —
# every one of them with commits on the base naming it, every one with no delivers: label.
# The poison also caused eleven ask beads to be filed questioning whether each should be
# dropped; all eleven were false and were closed during the operational recovery (sp-dj19i).
# \x1f is not IFS whitespace, so empty columns survive it. Verified directly:
#   printf 'a\tb\t\tc\n' | while IFS=$'\t' read -r w x y z; do echo "[$y]"; done   -> [c]
_c5_absent_repos=""
while IFS=$'\x1f' read -r id r_name superseded dropped sentcontent delivers started_at; do
    [ -n "$id" ] || continue
    # Carried from the delivers branch to the commit-naming check below, so a bead that
    # fails both is reopened once, naming both reasons.
    _c5_delivers_fail=""
    # Only beads an aeon worked — anything closed by hand has its own evidence.
    [ -f "$SPIRA_RUN/$id.log" ] || continue
    # A SUPERSEDED BEAD WILL NEVER HAVE A COMMIT NAMING IT, and that is correct: its work
    # was carried onto the successor's branch and lands under the successor's name. Without
    # this, two checks fought each other — the Sending sent the branch, and this check
    # then read the missing branch as work lost and reopened a bead that was deliberately
    # retired. `bd supersede` records the relation as a `supersedes` dependency; read it
    # rather than inventing a label for something the database already models.
    # Superseded comes out of the SAME query, which already carries dependencies. Asking
    # `bd show` per bead cost 83ms x 36 closed beads for a fact the list had already
    # returned — the third time tonight a per-item call was made for something a bulk
    # query had in hand.
    [ "$superseded" = 1 ] && continue
    # A BEAD THE OPERATOR DROPPED WILL NEVER HAVE A COMMIT NAMING IT EITHER, and that is
    # equally correct: the verdict was "do not do this work", so no branch and no commit is
    # ever coming. Without this, CHECK 5 reopens it every pass and the drop cannot stick —
    # sp-m56w was closed on Ryan's verdict at 00:17 on 2026-09-08 and reopened by this check
    # at 00:18:52, poison label intact, so no aeon would claim it and nothing would ever
    # land it. A permanent zombie reached by a check that was right about every other bead.
    [ "$dropped" = 1 ] && continue
    # A BRANCH THE SENDING REAPED BY CONTENT LEAVES NO COMMIT NAMING THE BEAD. content_landed
    # deletes a branch when merging it would produce exactly the base tree — the work is on the
    # base, but under some other commit, so no merge commit is ever made and the subject search
    # below finds nothing. The guard above it ("work exists on a branch, CHECK 6 lands it")
    # cannot fire either, because the Sending deleted that branch one pass earlier. So the bead
    # was reopened, re-worked from scratch by a fresh aeon, closed, reaped and reopened again:
    # 99 reopens over 80 beads in one day, each landing 86-143s after its own SENT — one
    # sentinel pass, no jitter. sp-637b went six rounds. The Sending now labels these
    # `content-landed` and this reads it.
    #
    # POISON IS TERMINAL HERE TOO. A poisoned bead was still being reopened by this check —
    # sp-637b was poisoned after 3 attempts and reopened as attempt 4 four minutes later — so
    # the counter that exists to bound the loop was being outrun by it.
    [ "${sentcontent:-0}" = 1 ] && continue
    # A BEAD CARRYING delivers:TYPE DECLARED WHAT IT PRODUCED INSTEAD OF A COMMIT. Verify
    # each declared output is actually present via delivers_verdict (lib.sh); if any
    # evidence is absent, reopen — a delivers: declaration with nothing behind it is a bead
    # closed on nothing, which is precisely what this check exists to catch. This supersedes
    # no-payload (sp-ail7), which exempted unconditionally.
    if [ -n "${delivers:-}" ]; then
        # delivers_verdict (lib.sh) is the one case block for delivers:TYPE evidence,
        # shared with aeon.sh's own post-session check (UC-aeon-execution-13: the two
        # decide delivers types identically because it is the same function). since-epoch
        # here is the bead's own started_at, converted once — CHECK5 judges a bead across
        # whatever window has passed since it started, not a session it is not running.
        _se=0
        [ -n "${started_at:-}" ] && _se="$(date -d "$started_at" +%s 2>/dev/null)" || _se=0
        _dv="$(delivers_verdict "$id" "$delivers" "${_se:-0}")"
        _delivers_ok="${_dv%%|*}"; _delivers_fail="${_dv#*|}"
        if [ "$_delivers_ok" = 1 ]; then
            log "CHECK5 $id: delivers ($delivers) verified — not reopened"
            continue
        fi
        # FALL THROUGH TO THE COMMIT-NAMING CHECK. Do not reopen here.
        #
        # delivers: and a commit are alternatives, not a replacement. incident.sh says so
        # where it writes the label: "When an Ops session commits code naming the bead, the
        # commit-naming check accepts the close and this label is never consulted." Reopening
        # here made the label authoritative instead, and the bead never reached the check
        # that would have accepted it.
        #
        # THE TOLERANCE IS THE POINT. The commit path leaves a bead closed while its branch
        # is merely ahead of the base — "work exists; CHECK 6 lands it" — because landing is
        # the Sending's job and takes minutes. This branch had no such tolerance, so a bead
        # that had done its work and was waiting to land was reopened and the work redone.
        #
        # SCAR: a bead closed with its fix committed, carrying a delivers:note: path nothing
        # had written because the fix was code and not a runbook application, was reopened 45
        # seconds after each close — three times, at $1.39, $0.23 and $0.67. The work was
        # correct every time and landed unchanged. At a fleet ceiling of one aeon it owned the
        # only slot for an hour. Fourteen open beads carried an unsatisfiable delivers: path
        # at the time, so the loop sat under every one of them.
        log "CHECK5 $id: delivers not verified ($_delivers_fail) — falling through to the commit check"
        _c5_delivers_fail="$_delivers_fail"
    fi
    r_path="$(repo_root "${r_name:-}")" || {
        case $'\n'"$_c5_absent_repos" in
            *$'\n'"$r_name"$'\n'*) ;;
            *) _c5_absent_repos+="${r_name}"$'\n'
               log "CHECK5: repo:$r_name is not in repo-map — skipping its closed beads" ;;
        esac
        continue; }
    # ONE `git log` PER REPO, not per bead. `landed` walks all commits on the base branch;
    # a 400-commit window caused beads older than that to be incorrectly marked unlanded and
    # reopened repeatedly (sp-a9g at 401, sp-37q at 400). This is `landed` inlined over a
    # cached walk (one dump of every subject on the base, reused for every closed bead in
    # this repository), so it must keep landed's THREE outcomes AND its match rule: a
    # landing record, never a mention. Dumping %B (full message) here and grepping the id
    # anywhere in it once matched any commit that named a bead in passing — a dependency
    # list, a "Fixes: <id> (analysis)" cross-reference — as that bead having landed, and
    # stranded five certified branches this way (sp-dgaig). %s (subject only) plus the same
    # two shapes lib.sh's landed() trusts keeps this check and the shared one from silently
    # drifting apart.
    if [ "$r_path" != "${subj_repo:-}" ]; then
        subj_repo="$r_path"
        # spira_landrefs is the base plus its local counterpart, both verified to resolve.
        # Non-zero means the repository cannot say what it lands on at all.
        subj_refs="$(spira_landrefs "$r_path")" || subj_refs=""
        subj_base="${subj_refs%% *}"
        # shellcheck disable=SC2086
        [ -n "$subj_refs" ] \
            && subjects="$(git -C "$r_path" log --format='%s' $subj_refs 2>/dev/null)" \
            || subjects=""
    fi
    # CANNOT TELL IS NOT "NOT LANDED". Reading an unresolvable base as "no commit names it"
    # would reopen every closed bead in that repository on every pass. The question is
    # unanswerable, so it is left unanswered and said out loud rather than answered wrongly.
    if [ -z "$subj_refs" ]; then
        log "CHECK5 $id: cannot resolve the ref $r_name lands on — not judging whether it landed"
    elif ! { grep -qxF "spira: land $id" <<< "$subjects" || grep -q "^${id}:" <<< "$subjects"; }; then
        # LANDSTATE CHECK. Read the pipeline record before deciding to reopen.
        _c5_ls_state=""; _c5_ls_tip=""
        if [ -r "$SPIRA_RUN/landstate/$id" ]; then
            read -r _c5_ls_state _c5_ls_tip _ < "$SPIRA_RUN/landstate/$id" 2>/dev/null || true
        fi
        # BATCHED/GATED/REBASED: a PR or the landing pass holds this work; don't reopen.
        case "${_c5_ls_state:-}" in
            BATCHED|GATED|REBASED)
                log "CHECK5 $id: landstate=$_c5_ls_state — pipeline handling it; not reopening"
                continue ;;
        esac
        # LANDED tip is an ancestor of the base: reached base via squash, amend, or content merge.
        if [ "${_c5_ls_state:-}" = LANDED ] && [ -n "${_c5_ls_tip:-}" ] \
               && [ "$_c5_ls_tip" != none ] && [ -n "${subj_base:-}" ] \
               && git -C "$r_path" merge-base --is-ancestor "$_c5_ls_tip" "$subj_base" 2>/dev/null; then
            log "CHECK5 $id: landstate LANDED, tip $_c5_ls_tip is ancestor of $subj_base — not reopening"
            continue
        fi
        # HOLD MODE: a closed bead with a standing branch is the expected terminal state.
        # The human has not merged it yet; this is not a fault.
        if [ "$(repo_land "${r_name:-}" 2>/dev/null)" = "hold" ]; then
            log "CHECK5 $id: repo ${r_name:-} is land=hold — standing branch is terminal; not reopening"
            continue
        fi
        # THE RECORDED AFFINITY, NOT THE ID-DERIVED DEFAULT. Sibling subtask beads normally
        # share one branch (bead_branch, lib.sh — law-branch-affinity-is-recorded); their own
        # refs/heads/spira/$id never exists, so a commit landed on the shared branch was
        # invisible here and the bead was reopened despite the commit matching CHECK 5's own
        # rule. bead_branch falls back to spira/$id when no affinity was ever recorded, so
        # this subsumes the plain own-branch case rather than replacing it.
        _c5_aff="$(bead_branch "$id")"
        if git -C "$r_path" show-ref --verify -q "refs/heads/$_c5_aff"; then
            # A COMMIT NAMING THE BEAD ON ITS AFFINITY BRANCH IS WORK-IN-FLIGHT even before
            # it is ahead of the base by CHECK 5's count — the same rule the subject walk
            # above applies to the base itself.
            if git -C "$r_path" log --format='%s' "$_c5_aff" 2>/dev/null | grep -q "^${id}:"; then
                log "CHECK5 $id: commit on affinity branch $_c5_aff matches ^${id}: — not reopening"
                continue
            fi
            # ZERO COMMITS AHEAD IS NOT WORK ON A BRANCH. An empty branch kept by the
            # Sending (content_landed now returns non-zero for zero-ahead) looks like "work
            # on a branch" from here, but has no commits to land and CHECK 6 cannot advance
            # it. Only exempt when the branch actually has commits of its own.
            #
            # ONLY ON THE BEAD'S OWN BRANCH. A shared affinity branch is ahead of the base
            # because of its SIBLINGS' commits, so counting them here would exempt every bead
            # recorded against a live shared branch, whether or not any commit is its own.
            # There the subject match above is the whole test; no match falls through to
            # the reopen.
            if [ "$_c5_aff" = "spira/$id" ]; then
                _c5_base="${subj_base:-}"
                _c5_ahead="$(git -C "$r_path" rev-list --count \
                    "${_c5_base:+${_c5_base}..}$_c5_aff" 2>/dev/null)" || _c5_ahead=0
                [ "${_c5_ahead:-0}" -gt 0 ] 2>/dev/null && continue  # work exists; CHECK 6 lands it
            fi
        else
            # BRANCH MISSING. Restore from the landstate tip or remote tracking ref so
            # landing.sh can pick it up — a deleted ref is not evidence of unlanded work
            # when the tip object still exists.
            _c5_restore_tip=""
            if [ -n "${_c5_ls_tip:-}" ] && [ "$_c5_ls_tip" != none ] \
                   && git -C "$r_path" cat-file -e "$_c5_ls_tip" 2>/dev/null; then
                _c5_restore_tip="$_c5_ls_tip"
            elif git -C "$r_path" show-ref --verify -q "refs/remotes/origin/spira/$id" 2>/dev/null; then
                _c5_restore_tip="$(git -C "$r_path" rev-parse "refs/remotes/origin/spira/$id" 2>/dev/null)" || true
            fi
            if [ -n "${_c5_restore_tip:-}" ]; then
                git -C "$r_path" update-ref "refs/heads/spira/$id" "$_c5_restore_tip" 2>/dev/null \
                    && log "CHECK5 $id: branch restored to $_c5_restore_tip (${_c5_ls_state:-unknown} landstate) — CHECK 6 will land it" \
                    || log "CHECK5 $id: branch restore failed"
                _c5_base="${subj_base:-}"
                _c5_ahead="$(git -C "$r_path" rev-list --count \
                    "${_c5_base:+${_c5_base}..}spira/$id" 2>/dev/null)" || _c5_ahead=0
                [ "${_c5_ahead:-0}" -gt 0 ] 2>/dev/null && continue
            elif [ "${_c5_ls_state:-}" = CERTIFIED ]; then
                # CERTIFIED but branch and tip are gone — can't restore; don't reopen.
                # The queue will skip it; a hand-recovery or a new aeon is needed.
                log "CHECK5 $id: landstate CERTIFIED, branch and tip gone — not reopening"
                continue
            fi
        fi
        # COUNT IT. A bead that closes itself without committing a working change is
        # reopened here, becomes ready, is claimed, and closes itself again — a loop
        # with no counter, which is precisely the loop the poison threshold exists to
        # bound. The aeon's own post-session check handles the normal case (the aeon
        # detects closed+uncommitted and reopens it, so the cleanup trap sees the bead
        # as open and charges via session_outcome). This is the safety net for the case
        # where the aeon exited before reaching that check — in which case no attempt
        # has been charged yet and this is genuinely a failed attempt at the work.
        # Counter labels (sp-attempt-N) no longer written; the events trail records
        # this reopening as a future attempt when the bead is next claimed (sp-lzt).
        bead_reopen "$id" closed-without-commit "Reopened by sentinel:${_c5_delivers_fail:+ ${_c5_delivers_fail}, and} closed, but no commit on ${subj_base:-the base} or on ${_c5_aff:-spira/$id} (recorded branch affinity) names it in $r_name. Closed is not landed; the next claim counts toward the poison threshold via the events trail. If this bead was closed because another bead did the work, record it with: bd supersede $id --with <successor> — a close reason alone is not read by this check."
        progress "reopened $id — closed without landing"
    fi
done < <(
    # EVERY PERSONA'S PARTITION, NOT THE BUILDER'S. This listed `--label spira,plan`, so a
    # bead of any other persona closed without a commit naming it was invisible to the one
    # check that exists to catch that — law-closed-is-not-landed, pointed at one partition
    # of several. A chamber that declares no partition is named below rather than read as a
    # clean sweep.
    home_repo="$(spira_home_repo)"
    while IFS=$'\t' read -r part _; do
        [ -n "$part" ] || continue
        bdjson list --status closed --limit 0 --label "$part" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
home = sys.argv[1]
for i in (d if isinstance(d, list) else [d]):
    repo = next((l[5:] for l in (i.get("labels") or []) if l.startswith("repo:")), home)
    # The third column is supersession, read from the dependencies this query already
    # returns rather than fetched per bead.
    #
    # `bd list` AND `bd show` NAME THE SAME FIELD DIFFERENTLY. show returns
    # {"dependency_type": "supersedes"}; list returns {"type": "supersedes"}. Reading only
    # the show spelling off a list row yields None for every dependency, so `sup` was 0 for
    # every bead and this exemption had never once fired: sp-dvlq was superseded by sp-35pl,
    # carried the dependency, and was still reopened as closed-without-landing every two
    # minutes. Accept either spelling rather than the one the neighbouring command happened
    # to use, because nothing here can tell which shape it was handed.
    sup = 1 if any((x.get("dependency_type") or x.get("type")) == "supersedes"
                   for x in (i.get("dependencies") or [])) else 0
    # Fourth column: dropped by the operator, carried as a label because "dropped" is not a
    # relation between beads the way supersession is — there is no second bead to point at.
    drop = 1 if "spira-dropped" in (i.get("labels") or []) else 0
    # Fifth column: the Sending reaped this branch by content, or the bead is poisoned. Both
    # mean no commit will ever name it, so reopening only burns another aeon on finished work.
    lab = i.get("labels") or []
    sentc = 1 if ("content-landed" in lab or "spira-poison" in lab) else 0
    # Sixth column: delivers:TYPE labels the aeon set on close — semicolon-separated list of
    # the values after "delivers:", e.g. "beads" or "note:/path/to/file". Empty if none. The
    # loop verifies each declared output is present; an empty column means this bead must have
    # a commit on the base (the normal path). This supersedes no-payload (sp-ail7).
    delivers = ";".join(l[len("delivers:"):] for l in lab if l.startswith("delivers:"))
    # Seventh column: started_at — the timestamp of the last claim, used by note/report checks
    # to confirm the file was written in the bead window, not before the session began.
    started = i.get("started_at") or ""
    # \x1f, not tab: an empty column between two tabs is eaten by bash read (see the loop).
    print("\x1f".join([i["id"], repo, str(sup), str(drop), str(sentc), delivers, started]))' "$home_repo" 2>/dev/null
    done <<< "$PARTITIONS" |
    # Sorted on the REPOSITORY column first, because the loop above caches one `git log`
    # walk per repository and re-walks whenever the repository changes between rows; `-u`
    # then drops the duplicate a bead carrying two personas' labels would produce. Both
    # keys together are the whole line, so nothing is deduplicated on a partial key.
    sort -u -t$'\x1f' -k2,2 -k1,1
)
[ -n "$PARTITIONS" ] || log "CHECK5 no persona in the chamber declares a partition — no closed bead is being checked for landing"
fi  # SPIRA_SKIP_CLOSED_CHECK

# ======================================================================================
# CHECK 6 — land finished branches. THE WORK IS NOT DONE HERE; it is dispatched to
# landing.sh and this pass moves on. Landing fetches, rebases, runs a repository's whole
# gate and pushes: Measured, an ordinary pass cost 21s and the one pass that
# landed cost 5m30s, during which CHECK 7 below could not run and a free aeon slot sat
# empty with 15 beads ready. Cheap deterministic work must not queue behind expensive work,
# and this is the check that made it.
#
# WHY NOT SIMPLY REORDER CHECK 7 ABOVE CHECK 6. It recovers the empty slot and nothing else.
# systemd will not start a second instance of a running oneshot, so a five-minute pass still
# swallows the ticks behind it and the loop's period is still set by its most expensive
# step — which is the actual defect. The decoupling is the fix; the reorder was its shadow.
#
# THE UNIT NAME IS THE MUTEX AND THERE IS NO LOCKFILE. systemd refuses to start a unit that
# is already active, so a pass arriving mid-landing declines and moves on. `--collect` is
# not tidiness: without it a FAILED transient unit stays loaded and every later
# `systemd-run --unit=spira-landing` is refused forever, which is a landing leg that stops
# dead and never says so.
#
# NOTHING IS WAITED ON, so nothing about the landing's outcome is known during this pass.
# What is known is what the PREVIOUS run left behind, and that is read in two places below:
# the mailbox, which carries the movements it made, and the status file, which is the
# positive control — because "nothing landed" and "the landing worker has not run since
# Tuesday" are indistinguishable from here unless the worker says which one it is
# (law-absence-needs-a-positive-control).
#
# THE ENVIRONMENT IS EXPLICIT, NOT INHERITED. systemd-run does not carry the caller's
# environment across, which is correct and is also what this must want: a sentinel that
# exported SPIRA_FAYTHS into a transient unit once had it reach a test suite asserting
# defaults, and correct work was rejected on every retry with nothing pointing at the
# environment (law-gates-run-in-a-clean-environment). Pass what landing.sh needs and
# nothing else — in particular not SPIRA_FAYTHS, which is this pass's business alone.
#
# 40% AND A RuntimeMaxSec, BECAUSE THIS LEAVES THE SENTINEL'S CGROUP. Landing ran under
# spira-sentinel.service's CPUQuota=40% until it was split out; a transient unit is its own
# cgroup, so without a quota of its own the split would quietly hand the box more Spira than
# it had before. 40% is the same ceiling it already had. This machine also runs prod, two CI
# runners and the operator's session, and anything that polls in a loop on it gets a quota before it
# is enabled (law-fence-loops-on-shared-hardware). RuntimeMaxSec is the knob that applies to
# a `simple` service, which is what systemd-run creates; TimeoutStartSec would be ignored.
# ======================================================================================
LAND_UNIT="${SPIRA_LAND_UNIT:-spira-landing}"
# THE CAP IS SIZED AGAINST THE GATE, AND THE WORKER IS TOLD WHAT IT IS. At 1800s this leg
# could not finish a single pass once anything closed: a full spira gate measured 776s cold
# on 2026-09-07 and far more under contention, so four consecutive passes were SIGTERMed
# mid-gate having moved nothing while two closed beads waited and the base ref went six hours
# without a commit. It was invisible until then because a pass with nothing to land finishes
# in under twenty seconds, so the cap was only ever approached on the one path that matters.
#
# Raising it is half the fix and the weaker half — a bigger number just moves the cliff. The
# other half is in landing.sh, which now refuses to BEGIN a gate it has not time to finish,
# so a pass ends cleanly and its successor continues rather than restarting the same gate
# forever. That is why the worker is given the number rather than left to guess it.
LAND_MAXSEC="${SPIRA_LAND_MAXSEC:-3600}"
LAND_STALE="${SPIRA_LAND_STALE:-1800}"      # seconds; a leg quieter than this is broken
LAND_STATUS="$SPIRA_RUN/landing.status"
LAND_MAILBOX="$SPIRA_RUN/landing.progress"

# systemctl behind a seam for the same reason `bd` and `gh` are: a suite has to be able to
# say "a landing is in flight" without one, and there is no other way to ask.
land_active() {
    [ "$("${SPIRA_SYSTEMCTL:-systemctl}" --user is-active "$LAND_UNIT.service" 2>/dev/null)" = active ]
}

# DRAIN BY RENAME. The worker appends while this reads, so the mailbox is moved aside first
# and read from the copy: a line is then counted exactly once, and a landing that finishes
# mid-drain simply lands its lines in the next pass's mailbox rather than in a file being
# consumed underneath it.
land_drain() {
    local mine="$SPIRA_RUN/landing.progress.drain.$$" f line
    [ -s "$LAND_MAILBOX" ] && mv -f "$LAND_MAILBOX" "$mine" 2>/dev/null
    # EVERY drain file, not just this pass's. One left behind by a pass that died between
    # the rename and the read holds movements the DAG really made, and a `progress` silently
    # dropped is a pass that judges itself starved when it was not — the same class of bug
    # as counting one twice, arriving from the other side. The glob is guarded because an
    # unmatched one expands to itself.
    for f in "$SPIRA_RUN"/landing.progress.drain.*; do
        [ -f "$f" ] || continue
        while IFS= read -r line; do
            [ -n "$line" ] || continue
            # THE SENTINEL COUNTS IT, not landing.sh. `progressed` gates CHECK 8, and a
            # movement the harness made is a movement whichever process made it — the pass
            # it is counted in is a detail of scheduling, not of the DAG.
            progress "$line"
        done < "$f"
        rm -f "$f"
    done
}

land_drain

# ADMISSION THROTTLE — update the stamp file and escalate on transitions. Reads depth from
# landstate and drain from LANDED records; writes $SPIRA_RUN/queue-throttled when active.
# CHECK7 reads that stamp to hold the task pool at 0. Lanes are never gated here.
[ -r "${SPIRA_HOME}/watchtower.sh" ] && \
    bash "${SPIRA_HOME}/watchtower.sh" --throttle-check 2>/dev/null || true

# CZAR OUTCOME CHECK — verify that czar-trigger beads are claimed promptly and that the
# conditions they were filed for have cleared after closure. Uses bd to query bead state;
# files one escalation per bead (deduped) when a bead is unclaimed or its condition returned.
[ -r "${SPIRA_HOME}/watchtower.sh" ] && \
    bash "${SPIRA_HOME}/watchtower.sh" --czar-outcome-check 2>/dev/null || true

# PR-MODE STALL CHECK — detect PRs that have been open past SPIRA_PR_STALL_MINS (default 60).
# Reads landstate files; makes GitHub API calls only when stalled PRs are found.
[ -r "${SPIRA_HOME}/watchtower.sh" ] && \
    bash "${SPIRA_HOME}/watchtower.sh" --pr-stall-check 2>/dev/null || true

# THE POSITIVE CONTROL, read before anything is launched so it describes a completed run
# rather than the one this pass is about to start.
land_age=-1
if [ -r "$LAND_STATUS" ]; then
    # shellcheck disable=SC1090
    eval "$(sed -n 's/^\(SP_LAND_[A-Z]*\)=\([0-9-]*\)$/\1=\2/p' "$LAND_STATUS" 2>/dev/null)"
    land_age=$(( $(date +%s) - ${SP_LAND_AT:-0} ))
    log "CHECK6: last landing ${land_age}s ago — rc=${SP_LAND_RC:-?}, ${SP_LAND_BRANCHES:-?} branch(es) seen, ${SP_LAND_MOVED:-?} moved"
    if [ "${SP_LAND_RC:-0}" != 0 ]; then
        log "CHECK6 WARN: the last landing exited ${SP_LAND_RC} — see $SPIRA_RUN/landing.log"
        land_escalate "its last run exited ${SP_LAND_RC}" \
            "$(printf 'STATUS  %s\n\n--- landing.log (tail) ---\n%s\n' \
                 "$(tr '\n' ' ' < "$LAND_STATUS")" "$(land_log_tail 30)")"
    fi
else
    log "CHECK6: no landing has ever completed on this host"
fi

if land_active; then
    log "CHECK6: a landing is already in flight — this pass does not start another"
elif "${SPIRA_LAUNCH:-systemd-run}" --user --collect --quiet \
        --unit="$LAND_UNIT" \
        --property=RuntimeMaxSec="$LAND_MAXSEC" \
        --property=CPUQuota=40% --property=Nice=10 \
        --property=StandardOutput="append:$SPIRA_RUN/landing.log" \
        --property=StandardError="append:$SPIRA_RUN/landing.log" \
        --setenv=PATH="$PATH" --setenv=HOME="$HOME" \
        --setenv=SPIRA_HOME="$SPIRA_HOME" --setenv=SPIRA_RUN="$SPIRA_RUN" \
        --setenv=SPIRA_DB="$SPIRA_DB" --setenv=SPIRA_REPO="${SPIRA_REPO:-}" \
        --setenv=SPIRA_REPO_MAP="$SPIRA_REPO_MAP" \
        --setenv=SPIRA_HOME_REPO="$(spira_home_repo)" \
        --setenv=SPIRA_BD="${SPIRA_BD:-bd}" --setenv=SPIRA_GH="${SPIRA_GH:-gh}" \
        --setenv=SPIRA_BATCH_MAXPAR="${SPIRA_BATCH_MAXPAR:-}" \
        --setenv=SPIRA_LAND_MAXSEC="$LAND_MAXSEC" \
        "$SPIRA_HOME/landing.sh" 2>/dev/null
then
    log "CHECK6: landing dispatched as $LAND_UNIT"
    # The FIRST dispatch ever, never overwritten. It is what makes "a worker that has never
    # once completed a run" a detectable state rather than a permanently silent one: without
    # it, a landing.sh that dies before it can write a status file leaves no status to be
    # stale, and the staleness check below would have nothing to measure against forever.
    [ -f "$SPIRA_RUN/landing.dispatched" ] || date +%s > "$SPIRA_RUN/landing.dispatched"
elif land_active; then
    # The race: a landing started between the check above and the launch. Declining is the
    # right answer and it is not a failure, so it must not be reported as one.
    log "CHECK6: a landing started underneath this pass — not starting another"
else
    log "CHECK6 WARN: could not dispatch the landing worker; nothing will land until this is fixed"
    land_escalate "the landing worker will not start" \
        "$(printf 'systemd-run --unit=%s refused, and the unit is not active.\n\n--- landing.log (tail) ---\n%s\n' \
             "$LAND_UNIT" "$(land_log_tail 30)")"
fi

# A leg that has not COMPLETED a run in half an hour, with nothing in flight, is broken —
# and this is the case the whole status file exists for. Checked after the dispatch so a
# host that has simply never run one is not escalated about on its very first pass.
if [ "$land_age" -lt 0 ] && [ -f "$SPIRA_RUN/landing.dispatched" ]; then
    land_age=$(( $(date +%s) - $(cat "$SPIRA_RUN/landing.dispatched" 2>/dev/null || date +%s) ))
fi
if ! land_active && [ "$land_age" -gt "$LAND_STALE" ]; then
    log "CHECK6 WARN: no landing has completed in ${land_age}s and none is running"
    land_escalate "nothing has completed a landing pass in ${land_age}s" \
        "$(printf 'STATUS  %s\n\n--- landing.log (tail) ---\n%s\n' \
             "$( [ -r "$LAND_STATUS" ] && tr '\n' ' ' < "$LAND_STATUS" || echo 'none — no run has ever written one' )" \
             "$(land_log_tail 30)")"
fi

# READ THE MAILBOX AGAIN. A landing that finished while this pass was running has movements
# to report and no reason to wait two minutes to be counted; in the normal case the dispatch
# above returned in milliseconds and this finds nothing.
land_drain

# CHECK 6c — REMOVED. The bespoke awaiting-ci sweep has been replaced by bd gate check
# running on spira-gate-check.timer. An aeon working on a pr-mode repository creates a
# gh:run gate (bd gate create --type=gh:run --blocks <id>) instead of applying the
# awaiting-ci label. The gate makes the bead not ready — no reader has to remember to
# exclude a label. gate-check.sh runs bd gate discover per pr-mode repository to match
# open gates to their GitHub run IDs, then bd gate check --type=gh:run to resolve gates
# whose run has completed. The bespoke sweep that operated here (CHECK 6c) is removed, not
# left dormant; its logic was replaced by bd's own gate primitives.

# CHECK 3b — queue-mode dep wait. A ready bead whose closed blocker has a CERTIFIED or
# BATCHED landstate has not yet landed on base. mark_queue_waiters applies
# SPIRA_QUEUE_WAIT_LABEL so fayth_ready excludes it; removes the label once the blocker
# reaches LANDED. close_landed_queue_waiters closes any bead that already carries LANDED in
# its landstate but still has the wait label — these have no branch left to land through the
# normal path and would otherwise stay open and invisible to fayths indefinitely.
mark_queue_waiters 2>/dev/null || true
close_landed_queue_waiters 2>/dev/null || true

# CHECK 7 — idle capacity. Ready work and a free aeon is the whole point of the system.
#
# EVERY FAYTH IS ASKED ITS OWN PREDICATE, AND EVERY FAYTH IS ASKED. This whole block used
# to sit inside `if [ "$ready" -gt 0 ]`, where `$ready` was a single hardcoded
# `--label spira,plan` count — the BUILDER's partition, standing in for every persona. Ops
# was therefore unreachable by construction: an incident arriving while no plan work was
# queued summoned nothing, and Ops could only ever wake when the builder had work, which is
# backwards for an on-call role. A fayth carries FAYTH_LABELS precisely so its partition is
# its own; the readiness question has to be asked through it.
#
# summon_fayth lives in lib.sh so the decision is one thing in one place — and so it can be
# exercised by test-fayth.sh, which the version inlined here could not be: everything else
# in a sentinel pass touches the real repository and the real database.
# ======================================================================================
# ONE POOL, DRAWN DOWN IN THE ORDER THE PERSONAS ARE NAMED. SPIRA_MAX_AEONS is that pool and
# was, until 2026-09-07, a configuration key nothing read — so every persona had a private
# cap and nothing coordinated them. The order is the priority: Ops is named first because an
# on-call persona that has to wait behind feature work is not on call.
#
# A HOST THAT SETS NO POOL BEHAVES EXACTLY AS BEFORE — an empty pool is passed as empty, and
# every cap below it is the persona's own.
# THE POOL IS FOR AEONS, NOT FOR THE PARTY. Party members travel with you and are summoned by
# their own units; only task personas are called for a fight and dismissed after it, and only
# they draw on this. `live` is counted the same way, over the task roster alone, or a running
# Ops would consume a slot it was never taking from.
TASK_FAYTHS="$(spira_task_fayths)"
if [ -n "${SPIRA_MAX_AEONS:-}" ]; then
    task_live=0; for f in $TASK_FAYTHS; do task_live=$((task_live + $(aeon_count "$f"))); done
    pool="$(ck7_pool "$SPIRA_MAX_AEONS" "$task_live")"
    log "CHECK7 pool: ${SPIRA_MAX_AEONS} slot(s), $task_live live, $pool free — order: $TASK_FAYTHS"
else
    pool=""
fi
# LANE FAYTHS DRAW FIRST. A lane is a partition with work no builder will ever take, and
# builders always have a queue — so whichever loop runs first takes every free slot, and
# the one that runs second is told the fleet is full. Running lanes first is what makes a
# starved ops or qa queue resolve on the next freed slot instead of waiting for the plan
# queue to empty, which never happens.
#
# THE FLEET CEILING STILL BINDS THEM. Lanes pass no pool argument, so SPIRA_MAX_AEONS does
# not apply — but SPIRA_MAX_LIVE_AEONS does, deliberately: every aeon draws on one shared
# five-hour account window whoever scheduled it, and that window is shared with the
# operator's own sessions. A lane takes the NEXT slot; it does not add one.
LANE_FAYTHS="$(spira_lane_fayths)"
[ -n "$LANE_FAYTHS" ] && log "CHECK7 lanes (${SPIRA_LANES:-none} declared): $LANE_FAYTHS"
# LANE ROTATION: rotate the evaluation order so all lanes get equal access to the
# collective cap over successive passes. State: the last summoned lane fayth name.
_lane_rr="$SPIRA_RUN/lane-round-robin"
_lane_last="$(cat "$_lane_rr" 2>/dev/null)"
LANE_FAYTHS="$(lane_rotate "$_lane_last" $LANE_FAYTHS)"
# WHEN INDIVIDUAL BD CALLS ARE SLOW, a partition the pass did not reach is absent from
# the log — absent looks identical to "nothing ready" in strand.sh. Track elapsed time
# and log remaining fayths as not-evaluated when the budget runs out so strand.sh can
# prefer "pass-truncated" over "starved".
_ck7_start="$(date +%s)"
_ck7_budget="${SPIRA_SENTINEL_PASS_BUDGET_SECS:-90}"
for f in $LANE_FAYTHS; do
    if [ $(( $(date +%s) - _ck7_start )) -ge "$_ck7_budget" ]; then
        log "CHECK7 $f: not evaluated (pass budget exhausted)"
        continue
    fi
    if summon_fayth "$f"; then
        act "summoned a $f lane aeon"
        printf '%s' "$f" > "$_lane_rr"
    fi
done

# THE POOL DRAWS ON WHAT IS LEFT. Recomputed after the lanes, because a lane summoned above
# consumes a slot the fleet ceiling counts, and a pool figure read before that is stale.
#
# ADMISSION THROTTLE: if --throttle-check engaged the stamp, hold the task pool at 0.
# Lane fayths are not affected — only builders draw from this pool, and the stamp says
# the queue is over capacity, not that lane work is unneeded (sp-h7zzx).
_tc_stamp_ck7="${SPIRA_THROTTLE_STAMP:-$SPIRA_RUN/queue-throttled}"
_ck7_express_label=""
_ck7_stamp_exists=0; [ -f "$_tc_stamp_ck7" ] && _ck7_stamp_exists=1
if [ "$(ck7_throttled "$_ck7_stamp_exists" "${SPIRA_QUEUE_THROTTLE_OVERRIDE:-}")" = 1 ]; then
    _ck7_express_ready=0
    express_ready_in_task_pool "$TASK_FAYTHS" "${SPIRA_EXPRESS_LABEL:-express}" && _ck7_express_ready=1
    pool="$(check7_pool_decision 1 "${pool:-0}" "$_ck7_express_ready")"
    if [ "$_ck7_express_ready" = 1 ]; then
        _ck7_express_label="${SPIRA_EXPRESS_LABEL:-express}"
        log "CHECK7 pool: throttle active — express bead ready, granting pool=$pool (restricted to '$_ck7_express_label')"
    else
        log "CHECK7 pool: throttle active ($(head -1 "$_tc_stamp_ck7" 2>/dev/null)) — task pool held at $pool"
    fi
fi
for f in $TASK_FAYTHS; do
    if [ $(( $(date +%s) - _ck7_start )) -ge "$_ck7_budget" ]; then
        log "CHECK7 $f: not evaluated (pass budget exhausted)"
        continue
    fi
    _fill=0
    while summon_fayth "$f" "$pool" "$_ck7_express_label"; do
        act "summoned a $f aeon"
        [ -n "$pool" ] && pool=$(( pool > 0 ? pool - 1 : 0 ))
        _fill=$(( _fill + 1 ))
        [ "$(ck7_fill_cap "$_fill" "$pool")" = stop ] && break
        [ $(( $(date +%s) - _ck7_start )) -ge "$_ck7_budget" ] && break
    done
done

# CHECK 6b MOVED BELOW CHECK 7: the Sending walks branches with git and bd calls and took
# 4-7 minutes a pass, and the summon decision waited behind it. Summon first; reap after.
# CHECK 6b — the Sending. Send the branch and worktree of every bead whose work is now an
# ancestor of its repository's base.
#
# sending.sh judges by ancestry alone, never by bead status, so it cannot be talked into
# deleting work by a database that is merely optimistic.
#
# BASE-UNCHANGED SKIP. When no repo's land ref has moved since the last walk, nothing could
# have landed. Stamp: per-repo name=sha lines at $SPIRA_RUN/sending.base, written after
# each full walk. Repos that repo_root cannot resolve are skipped in both directions.
# ======================================================================================
_sending_base_stamp="$SPIRA_RUN/sending.base"
_sending_skip=0
if [ -f "$_sending_base_stamp" ]; then
    _sending_all_match=1
    while IFS='=' read -r _sr_name _sr_sha; do
        [ -n "$_sr_name" ] || continue
        _sr_repo="$(repo_root "$_sr_name" 2>/dev/null)" || { _sending_all_match=0; break; }
        _sr_ref="$(spira_landref "$_sr_repo" 2>/dev/null)" || { _sending_all_match=0; break; }
        _sr_cur="$(git -C "$_sr_repo" rev-parse "$_sr_ref" 2>/dev/null)" || { _sending_all_match=0; break; }
        [ "$_sr_cur" = "$_sr_sha" ] || { _sending_all_match=0; break; }
    done < "$_sending_base_stamp"
    if [ "$_sending_all_match" -eq 1 ]; then
        while IFS= read -r _sr_name; do
            [ -n "$_sr_name" ] || continue
            repo_root "$_sr_name" >/dev/null 2>&1 || continue
            spira_landref "$(repo_root "$_sr_name" 2>/dev/null)" >/dev/null 2>&1 || continue
            grep -qF "${_sr_name}=" "$_sending_base_stamp" 2>/dev/null \
                || { _sending_all_match=0; break; }
        done < <(spira_repos 2>/dev/null)
    fi
    [ "$_sending_all_match" -eq 1 ] && _sending_skip=1
fi
if [ "$_sending_skip" -eq 1 ]; then
    log "sending: base unchanged — skipped"
else
    sent="$("$SPIRA_HOME/sending.sh" 2>&1)"
    [ -n "$sent" ] && printf '%s\n' "$sent"
    n_sent="$(grep -c '^SENT' <<< "$sent" || true)"
    if [ "${n_sent:-0}" -gt 0 ]; then
        while read -r _ rid rrepo rbr _; do
            [ -n "${rbr:-}" ] || continue
            act "sent $rrepo $rbr $rid"
        done < <(grep '^SENT' <<< "$sent")
    fi
    grep -q '^FAILED' <<< "$sent" && log "sending reported a branch it could not delete"
    { for _sr_name in $(spira_repos 2>/dev/null); do
        _sr_repo="$(repo_root "$_sr_name" 2>/dev/null)" || continue
        _sr_ref="$(spira_landref "$_sr_repo" 2>/dev/null)" || continue
        _sr_cur="$(git -C "$_sr_repo" rev-parse "$_sr_ref" 2>/dev/null)" || continue
        printf '%s=%s\n' "$_sr_name" "$_sr_cur"
    done; } > "$_sending_base_stamp"
fi

# ======================================================================================
# CHECK 7c — ready beads no persona can claim. Every partition reporting "nothing ready"
# is ambiguous: the queue may be genuinely empty, or a bead may be present with labels
# that prevent every persona from claiming it. CHECK 7 cannot distinguish these — it asks
# each fayth's own predicate and stops at 0. This check reads the raw ready set, tests
# each bead against the full chamber, and surfaces any with an empty intersection.
#
# THE TWO FAILURE MODES this detects:
#   1. fayth:<persona> with partition labels the named persona does not own — the fifteen-
#      hour strand of 2026-09-09: seven P1 beads carried fayth:ops on spira,plan labels;
#      builder matched the partition but was excluded by the preference; ops was excluded
#      by its own partition. Every persona reported 0; every report was truthful.
#   2. spira with no partition label — a bead any partition requires exactly one of (plan,
#      incident, ...) but carries none of them; invisible to every persona by construction.
#      Live instance at time of fix: sp-bvo7.
#
# THIS IS NOT AN ACTION — it does not change the DAG; it names what is wrong so the fix
# is one label, not a debugging session. Counted as `acted` so the pass summary says
# something surfaced rather than ending silently.
#
# Skipped when SPIRA_SKIP_RECLAIM=1: the raw-ready query costs ~400ms and fixture beads
# are labelled correctly by construction, so this check finds nothing and only costs time.
# ======================================================================================
if [ "${SPIRA_SKIP_RECLAIM:-0}" != 1 ]; then
unclaimable_out="$(detect_unclaimable_ready 2>/dev/null)"
if [ -n "$unclaimable_out" ]; then
    printf '%s\n' "$unclaimable_out"
    n_unc="$(grep -c '^UNCLAIMABLE' <<< "$unclaimable_out" || true)"
    log "CHECK7c: $n_unc ready bead(s) no persona can claim — fix each by adding or removing the label named above"
    act "surfaced $n_unc unclaimable ready bead(s)"
    # FILE ONE INCIDENT PER UNCLAIMABLE BEAD. The sentinel surfacing the finding in the log
    # is only as visible as the log; an incident bead is work Ops can claim and fix. incident.sh
    # dedupes on unclaimable:<id>, so a bead still stuck on the next pass gets a recurrence
    # count, not a duplicate bead (law-dedup-must-be-measured).
    file_unclaimable_incidents "$unclaimable_out"
fi
fi  # SPIRA_SKIP_RECLAIM

# ======================================================================================
# CHECK 7d — open beads whose recorded branch: is checked out in a DIFFERENT bead's
# worktree. aeon.sh's law-one-aeon-one-worktree refusal at claim time is correct; what it
# cannot do is stop the NEXT summon, because nothing about the input changes between
# claims (law-a-retry-must-change-an-input). Unlike CHECK 7c's label mismatches, the remedy
# here is mechanical, not a human judgement call — so this parks the bead directly with
# $SPIRA_ASK_LABEL rather than waiting on a Groomer incident.
# ======================================================================================
if [ "${SPIRA_SKIP_RECLAIM:-0}" != 1 ]; then
collision_out="$(detect_branch_collisions 2>/dev/null)"
if [ -n "$collision_out" ]; then
    printf '%s\n' "$collision_out"
    n_col="$(grep -c '^COLLISION' <<< "$collision_out" || true)"
    log "CHECK7d: $n_col bead(s) whose recorded branch is held by another bead's worktree — parking with $SPIRA_ASK_LABEL"
    act "parked $n_col branch-collision bead(s)"
    park_branch_collisions "$collision_out"
fi
fi  # SPIRA_SKIP_RECLAIM

if [ "$GOAL_REACHED" = 1 ]; then
    log "pass complete — $acted action(s), $progressed progress, goal reached"
    exit 0
fi

# ======================================================================================
# CHECK 8 — JUDGEMENT. Everything above passed, and there is no rule left to apply unless
# nothing progressed and the plan is starved: check8_should_judge(plan_ready, plan_inprog,
# n_open, progressed, last, now, every) alone decides — the plan's own readiness gates it
# (a ready INCIDENT says nothing about the plan and must not silence it), and rate-limiting
# keeps inference, a cost centre, from reasoning every minute about nothing.
# ======================================================================================
_ck8_now="$(date +%s)"
_ck8_last=0; [ -f "$COOLDOWN" ] && _ck8_last="$(cat "$COOLDOWN" 2>/dev/null || echo 0)"
case "$(check8_should_judge "$plan_ready" "$plan_inprog" "$n_open" "$progressed" \
            "$_ck8_last" "$_ck8_now" "$INFERENCE_EVERY")" in
    cooldown)
        log "starved, but inference is in cooldown ($(( INFERENCE_EVERY - _ck8_now + _ck8_last ))s left)"
        log "pass complete — $acted action(s), $progressed progress"
        exit 0
        ;;
    yes)
        echo "$_ck8_now" > "$COOLDOWN"
        log "STARVED — $n_open open, 0 ready, 0 running. Dropping to inference."
        "$SPIRA_HOME/reflect.sh" "$open_children" >> "$SPIRA_RUN/reflect.log" 2>&1
        act "invoked reflection"
        ;;
esac

log "pass complete — $acted action(s), $progressed progress"
