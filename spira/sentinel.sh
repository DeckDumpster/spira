#!/usr/bin/env bash
#
# sentinel.sh — the outer harness: compare current state to goal state, close the gap.
#
#   sentinel.sh                one pass (this is what the 2-minute timer runs)
#   sentinel.sh --report       print the gap, change nothing
#   sentinel.sh --summon-only  CHECK 7 alone, nothing else (the 15s timer, and every aeon
#                              unit's own ExecStopPost — a freed slot should refill in
#                              seconds, not wait for the next full pass; sp-0y2av)
#   sentinel.sh --audit        the decoupled worker CHECK 4's dispatch starts (sp-994y9): the
#                              per-bead/per-branch walks (poison, closed-not-landed, the
#                              Sending, unclaimable-ready, branch collisions), run in their
#                              own transient unit so a normal pass — reclaim, land-dispatch,
#                              summon — finishes in seconds regardless of how long the walk
#                              takes.
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
. "$(dirname "$0")/lc.sh"

REPORT=0; [ "${1:-}" = "--report" ] && REPORT=1
SUMMON_ONLY=0; [ "${1:-}" = "--summon-only" ] && SUMMON_ONLY=1
# AUDIT — this invocation is the decoupled worker (sp-994y9), not a normal pass. See the
# "AUDIT PASS DECOUPLING" block below for why this exists and what it skips.
AUDIT=0; [ "${1:-}" = "--audit" ] && AUDIT=1
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
# AUDIT MAILBOX: the decoupled worker's `progress` also appends the raw line here, drained
# and re-counted by the NEXT normal pass's own `progress` (audit_drain, below) — the exact
# mailbox shape CHECK 6 already uses for landing, because `progressed` here is per-process
# and CHECK 8 must see what an async worker did, not just this pass.
AUDIT_MAILBOX="${SPIRA_AUDIT_MAILBOX:-$SPIRA_RUN/audit.progress}"
if [ "$AUDIT" = 1 ]; then
    progress() { progressed=$((progressed+1)); act "$@"; printf '%s\n' "$*" >> "$AUDIT_MAILBOX"; }
else
    progress() { progressed=$((progressed+1)); act "$@"; }
fi

# ======================================================================================
# --summon-only — CHECK 7 alone. The full pass below costs a median 213s (max 537s, measured
# 2026-09-28) because CHECK7 runs near its END, behind goal/plan state reads, the Sending and
# CHECK7c/d; a slot an exiting aeon just freed sat empty for minutes waiting for the next full
# pass. This path reaches the summon loop directly, paying only what CHECK 7 itself needs:
# the world/capacity gate (files, no bd call), a live count for the pool arithmetic, and ONE
# bd ready call bucketed per persona (bulk_ready_by_fayth) rather than fayth_ready's own
# one-call-per-partition. ck7_summon_pass holds summon.lock either way, so this path and the
# full pass below can never both summon into the same freed slot (sp-0y2av).
# ======================================================================================
if [ "$SUMMON_ONLY" = 1 ]; then
    world_gate fleet summon-only || exit 0
    if capacity_paused; then
        log "summon-only: account out of capacity for another ${SPIRA_CAPACITY_LEFT}s — not summoning"
        exit 0
    fi
    live=0; for f in $FAYTHS; do live=$((live + $(aeon_count "$f"))); done
    log "summon-only: live=$live fayths=[$FAYTHS]"
    SPIRA_READY_CACHE="$(ready_cache_populate "$SPIRA_RUN")" || SPIRA_READY_CACHE=""
    [ -n "$SPIRA_READY_CACHE" ] && export SPIRA_READY_CACHE
    ck7_summon_pass
    rm -f "$SPIRA_READY_CACHE"
    log "summon-only pass complete — $acted action(s)"
    exit 0
fi

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

# EVERYTHING FROM HERE THROUGH CHECK 3 IS THE NORMAL PASS ONLY. The audit worker (sp-994y9)
# is a second process started from CHECK 4's dispatch, below — reclaim, pilgrimage-closing
# and the goal-completion state this span computes are the normal pass's job already done
# by the time the audit worker starts a moment later, and re-doing them here would race the
# same reclaim against itself under two PIDs for nothing gained.
if [ "$AUDIT" = 0 ]; then
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

# PER-CHECK WALL TIME (run/tsd/'s sentinel-phase family, law-producers-stamp-their-own-
# clock). _phase <name> flushes the CHECK that just ended and starts the clock for <name>,
# so every second of the pass belongs to exactly one row and an unlogged stretch (the design
# named "about 75s before CHECK4" and "the post-sending tail") becomes visible as part of
# whichever CHECK precedes it, rather than vanishing between two log lines.
PASS_ID="${HOSTNAME:-$(hostname 2>/dev/null || printf unknown)}-$$-$EPOCHSECONDS"
_PHASE_NAME="setup"
_PHASE_T0=$EPOCHSECONDS
_phase() {
    _tsd_sentinel_phase "$PASS_ID" "$_PHASE_NAME" "$(( EPOCHSECONDS - _PHASE_T0 ))"
    _PHASE_NAME="$1"
    _PHASE_T0=$EPOCHSECONDS
}

# ======================================================================================
# ONE STORE READ PER PASS (sp-bo67y). strand.sh, dispatchable_open and check4_closed_branched
# each asked bd once per partition below, and mark_queue_waiters, detect_unclaimable_ready
# and every fayth_ready call inside CHECK 7's summon loop asked bd ready again independently
# — roughly 30 bd list/ready calls in one pass, measured 05:43:23 2026-09-28 (375s wall,
# 69.8s CPU; check4_closed_branched alone pulled ~10.5MB of closed JSON per partition).
# Fetch each shape ONCE here; every consumer named above reads the cached JSON instead of
# asking bd again. Cleaned up by the trap regardless of which exit path below fires.
#
# SPIRA_READY_SNAPSHOT is the BROADEST ready set (ready_raw_args — no SPIRA_SCOPE_LABEL
# filter), because detect_unclaimable_ready must see a bead missing the scope label to
# report it; every scope- or partition-restricted consumer narrows it in-process instead
# (bulk_ready_by_fayth via each fayth's own FAYTH_LABELS, mark_queue_waiters and strand.sh
# via an explicit filter).
#
# SPIRA_READY_CACHE is bulk_ready_by_fayth's own per-fayth bucketing of that same snapshot —
# populated here too, not only under --summon-only (sp-0y2av), so every fayth_ready call
# CHECK 7's summon loop makes (the elastic and lane last-slot checks, and the final
# readiness check per persona) reads the cache fayth_ready already knows how to use instead
# of paying its own `bd ready` call.
# ======================================================================================
SPIRA_LIST_SNAPSHOT="$(mktemp "$SPIRA_RUN/list-snapshot.XXXXXX" 2>/dev/null)" || SPIRA_LIST_SNAPSHOT=""
SPIRA_READY_SNAPSHOT="$(mktemp "$SPIRA_RUN/ready-snapshot.XXXXXX" 2>/dev/null)" || SPIRA_READY_SNAPSHOT=""
SPIRA_READY_CACHE=""
if [ -n "$SPIRA_LIST_SNAPSHOT" ]; then
    bdjson list --all --limit 0 > "$SPIRA_LIST_SNAPSHOT" 2>/dev/null
    export SPIRA_LIST_SNAPSHOT
fi
if [ -n "$SPIRA_READY_SNAPSHOT" ]; then
    _snap_ready_args=(); while IFS= read -r _snap_arg; do _snap_ready_args+=("$_snap_arg"); done < <(ready_raw_args)
    bdjson "${_snap_ready_args[@]}" > "$SPIRA_READY_SNAPSHOT" 2>/dev/null
    export SPIRA_READY_SNAPSHOT
    SPIRA_READY_CACHE="$(ready_cache_populate "$SPIRA_RUN")" || SPIRA_READY_CACHE=""
    [ -n "$SPIRA_READY_CACHE" ] && export SPIRA_READY_CACHE
fi
trap 'rm -f "${SPIRA_LIST_SNAPSHOT:-}" "${SPIRA_READY_SNAPSHOT:-}" "${SPIRA_READY_CACHE:-}"' EXIT

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
_phase "CHECK1"
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
# SPIRA_SKIP_RECLAIM=1 bypasses this check and CHECK 2c. Each spira-lc call costs real
# wall time and a fixture that creates no stale leases pays that on every pass with nothing
# to show for it. Test suites that cover CHECK 4 (the poison valve) rather than reclaim
# behaviour set this to halve the per-pass wall time.
#
# ONE spira-lc SCAN FOR EVERY PARTITION AT ONCE (sp-i2m7y). check2_reclaim_stale (lib.sh)
# reads every WORKING row in spira_lifecycle in a single call — there is no partition
# dimension to loop over, so an ops or spike aeon that died is reaped by the same pass that
# reaps a builder's, with no per-persona label to remember to add. The /proc ghost sweep in
# CHECK 2b catches the same case faster in practice; this is the time-based backstop for
# whatever /proc cannot see.
_phase "CHECK2"
if [ "${SPIRA_SKIP_RECLAIM:-0}" != 1 ]; then
check2_protect_waiting
check2_reclaim_stale
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
_phase "CHECK2b"
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
# one. Both are about a lease. This was about a bead whose STATUS was already reset — by a
# reopen in the landing pass, in CHECK 5, or in the aeon's own closed-without-a-commit
# check — while its assignee was left standing. `bd ready` counts such a bead and `bd ready
# --claim` skips it, so it is ready forever and claimable never, and no lease ever expires
# to rescue it. Thirteen plan beads were stuck this way on 2026-09-06 while CHECK 7 summoned
# an aeon every two minutes to report idle within one second.
#
# MOVED ONTO spira-lc (sp-i2m7y): check2c_lc_consistency (lib.sh) reads spira_lifecycle,
# never bd, and finds nothing to release — the state/holder desync this check chased cannot
# occur there, because both fields change together in one version-checked transaction. It
# detects an anomaly rather than repairing one; see the function's own comment.
#
# Skipped when SPIRA_SKIP_RECLAIM=1 — see CHECK 2 above.
# ======================================================================================
_phase "CHECK2c"
if [ "${SPIRA_SKIP_RECLAIM:-0}" != 1 ]; then
inconsistent="$(check2c_lc_consistency)"
if [ -n "$inconsistent" ]; then
    printf '%s\n' "$inconsistent"
    n_inc="$(grep -c '^INCONSISTENT' <<< "$inconsistent" || true)"
    log "CHECK2c: $n_inc spira-lc row(s) with holder/state out of sync — a bug reached spira_lifecycle outside its own CAS"
    act "surfaced $n_inc inconsistent spira-lc row(s)"
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
_phase "CHECK3"
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
fi  # AUDIT (STATE .. CHECK 3)

# ======================================================================================
# CHECK 4/5/6b/7c/7d — AUDIT PASS DECOUPLING (sp-994y9). Everything from here through
# CHECK 7d is a walk over every dispatchable bead, every closed-unlanded bead, every
# branch, or every ready bead — none of it is O(1), and their combined cost is why a pass
# took 4-5 minutes against a 2-minute timer while CHECK 7 (summon) sat behind them: a
# fleet slot that freed up mid-walk waited for the WHOLE pass to finish before anything
# could refill it, exactly the shape CHECK 6 already solved for landing below. The fix is
# the same one: dispatch this span as its own transient unit and let the normal pass —
# reclaim, land-dispatch, summon — finish in seconds regardless of how long the walk
# takes. `sentinel.sh --audit` (this same file, AUDIT=1) is that worker; the dispatch
# block below it starts it and does not wait.
#
# NOTHING HERE DEPENDS ON STATE OR CHECK 1-3. dispatchable_open, check4_bulk_data and the
# rest read the bead graph directly; they do not read $plan_ready, $n_open or
# $GOAL_REACHED, which is what makes running them in a second process safe — the normal
# pass has already done reclaim and pilgrimage-closing before this dispatches, and this
# span needs nothing else from it.
# ======================================================================================
if [ "$AUDIT" = 1 ]; then
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
# exit path anyway. The poison hold itself is enough: once the round that makes the dispatch
# predicate read spira-lc holds lands (a companion cutover bead, not this one), the moment
# the holder lets go CHECK 7 stops summoning for it on that same fact.
# ======================================================================================
_phase "CHECK4"
dispatchable="$(dispatchable_open)"
log "CHECK4 examining $(printf '%s' "$dispatchable" | grep -c . || true) dispatchable bead(s), poison=$POISON_AT requeue=$REQUEUE_AT reclaim=$RECLAIM_AT"
# ONE QUERY FOR ALL ATTEMPTS, REOPENS AND RECLAIMS. dispatchable_open already parsed each
# bead's labels and emits them alongside the id; CHECK 4 reads them from there, not the
# store. attempts_of/reopens_of/reclaims_of are per-bead SQL calls; one GROUP BY returns
# the same numbers for the whole dispatchable set. On a 59-bead set: 177 calls / 29,731 ms
# → 1 call / 178 ms.
declare -A _c4_attempts _c4_reopens _c4_reclaims
# CAPTURED, NOT PIPED INTO THE LOOP DIRECTLY, so its exit status survives to be checked below
# — a process substitution's own exit status is invisible to the `while` that reads it, even
# under `pipefail`. A nonzero exit means the bulk query failed (check4_bulk_data's own log
# line already said why); the per-bead loop is skipped entirely rather than run against
# associative arrays that came back empty, which would read as "zero attempts" for every bead
# and silently poison/clear/requeue/reclaim off a false floor (sp-rp4g4).
_c4_bulk_out="$(check4_bulk_data "$dispatchable")"; _c4_bulk_rc=$?
if [ "$_c4_bulk_rc" -ne 0 ]; then
    log "CHECK4 bulk attempts query failed (rc=$_c4_bulk_rc) — making no poison/requeue/reclaim decision this pass"
fi
while IFS=$'\t' read -r _bid _batt _brep _brcl; do
    [ -n "$_bid" ] || continue
    _c4_attempts["$_bid"]="${_batt:-0}"
    _c4_reopens["$_bid"]="${_brep:-0}"
    _c4_reclaims["$_bid"]="${_brcl:-0}"
done <<< "$_c4_bulk_out"
[ "$_c4_bulk_rc" -eq 0 ] && while IFS=$'\t' read -r id _labels; do
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
    _pl_lifted=0; poison_lifted "$id" "$n" && _pl_lifted=1
    # THE POISON HOLD, READ FROM SPIRA-LC, NOT THE bd LABEL (sp-i2m7y). This is the read
    # dispatchable_open no longer excludes on, so an already-poisoned bead reaches this loop
    # every pass instead of being invisible to it — check4_decide's `clear` branch is what
    # keeps it from being poisoned again here.
    _poisoned=0; lc_held "$id" poison && _poisoned=1
    decision="$(check4_decide "$n" "$_requeues" "$_reclaims" "$_labels" "$_rq_asked:$_rc_asked:$_po_asked:$_pl_lifted" "$_poisoned")"

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
            # THE ONLY WRITE IS THE SPIRA-LC HOLD (sp-i2m7y): the spira-poison bd label sp-
            # ki12s kept as a dual write is gone — dispatchable_open no longer excludes on
            # it and check4_decide's "already poisoned" branch reads $_poisoned (lc_held
            # above), not a label, so nothing left in this pass ever reads or writes it.
            case " $decision " in *' poison '*)
                lc_hold "$id" poison "poisoned after $n in_progress transition(s) without landing" sentinel || true
                bdq note "$id" "Poisoned after $n in_progress transition(s) without landing. Triaged by the groomer, not a human: it reads the charged sessions' final results and either credits the harness-caused attempts and lifts the poison (groomer.sh unpoison) or splits/re-scopes the work. Any live holder keeps its claim and releases on its own exit path; no persona can claim it again while the hold stands." >/dev/null 2>&1
                progress "poisoned $id after $n attempts"
                # check4_decide only emits `poison` on the transition into poisoned (the
                # $_poisoned=0 branch), so this fires once — the bead keeps the hold, and
                # every later pass takes the other branch.
                spira_event bead.poisoned "$id" "poisoned $id after $n attempts" \
                    "the groomer triages it, not a human" || true
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
                # NAMES THE TOOL, NOT THE LABEL. `bd label remove` alone does not stick — CHECK
                # 4 re-poisons on the very next pass from the unchanged count (sp-qd2ul) —
                # attempts.sh clear also records the event the count is floored on.
                _po_dflt="if the work is correct, re-label or split the bead and run attempts.sh clear $id --apply; if it is not worth doing, close it"
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
_phase "CHECK4_SUPPLEMENT"
_c4_closed="$(check4_closed_branched)"
if [ -n "$_c4_closed" ]; then
    declare -A _c4c_reopens
    _c4c_bulk_out="$(check4_bulk_data "$_c4_closed")"; _c4c_bulk_rc=$?
    if [ "$_c4c_bulk_rc" -ne 0 ]; then
        log "CHECK4-closed bulk attempts query failed (rc=$_c4c_bulk_rc) — making no requeue decision this pass"
    fi
    while IFS=$'\t' read -r _bid _batt _brep _brcl; do
        [ -n "$_bid" ] || continue
        _c4c_reopens["$_bid"]="${_brep:-0}"
    done <<< "$_c4c_bulk_out"
    [ "$_c4c_bulk_rc" -eq 0 ] && while IFS=$'\t' read -r id _labels; do
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
        # ANCESTRY, NOT ONLY THE SUBJECT. landed() only recognises the queue's own subject
        # shapes; a hand-resolved merge that reaches the base under git's own default
        # subject (a bare "Merge branch ..." with no bead id at all) is invisible to it, and
        # a widened regex still cannot read an id that was never written. The landstate tip
        # is the same ancestry proof CHECK 5 trusts below — ask it here too, before mailing
        # the operator about work that is already on the base (sp-2exzw).
        if [ -n "$_r_path" ] && [ -r "$SPIRA_RUN/landstate/$id" ]; then
            _c4_ls_state=""; _c4_ls_tip=""
            read -r _c4_ls_state _c4_ls_tip _ < "$SPIRA_RUN/landstate/$id" 2>/dev/null || true
            if [ "${_c4_ls_state:-}" = LANDED ] && [ -n "${_c4_ls_tip:-}" ] && [ "$_c4_ls_tip" != none ]; then
                _c4_base="$(spira_landref "$_r_path" 2>/dev/null)" || _c4_base=""
                if [ -n "$_c4_base" ] \
                        && git -C "$_r_path" merge-base --is-ancestor "$_c4_ls_tip" "$_c4_base" 2>/dev/null; then
                    log "CHECK4-closed $id: landed by ancestry (landstate tip on $_r_name) — no escalation"
                    continue
                fi
            fi
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

# STALE POISON CLEAR. A poison hold is applied when a bead's attempt count reaches the
# threshold; the operator clears it after changing the approach. But the hold also becomes
# stale when someone removes attempt labels and the count drops below the threshold, so this
# scan finds it independently of whether it is ever otherwise re-examined (defect sp-9szt,
# and the mechanism that made it circular under the bd label — dispatchable_open excluding a
# poisoned bead from the very set CHECK 4 walks — no longer applies: sp-i2m7y).
#
# Scan all poisoned non-closed beads. Any whose count is now below the threshold is
# released: the condition that warranted the hold is gone.
# THE POISON SET COMES FROM SPIRA-LC (sp-i2m7y), not a bd label scan. EVENTS-BASED COUNT:
# attempt count comes from status_changed events, not labels (sp-lzt). Each poisoned bead is
# queried individually for its status/type — the per-bead cost is one SQL call each. This
# scan iterates the (usually small) poisoned set, not the whole dispatchable set — the N+1
# query sp-f1m7f removed is the CHECK4 main loop above, not this one.
#
# FAIL CLOSED, NOT OPEN. attempts_of prints nothing and returns nonzero on a query error; a
# poisoned bead this pass cannot read a count for is left exactly as it is, not cleared on a
# false zero that the next pass's bulk query (CHECK4 main loop above) would only re-poison a
# moment later (law-a-control-that-cannot-check-must-refuse).
while read -r id; do
    [ -n "$id" ] || continue
    _cst_json="$(bdjson show "$id" 2>/dev/null)" || _cst_json=""
    _cst="$(printf '%s' "$_cst_json" | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
    i = (d if isinstance(d, list) else [d])[0]
except Exception:
    print(""); raise SystemExit
print(i.get("status","") + "\t" + str(i.get("issue_type","")))' 2>/dev/null)"
    _cst_status="${_cst%%$'\t'*}"; _cst_type="${_cst#*$'\t'}"
    [ "$_cst_status" = closed ] && continue
    case "$_cst_type" in epic|event) continue ;; esac
    if ! n="$(attempts_of "$id")"; then
        log "CHECK4 $id: attempts query failed — stale-poison-clear makes no decision this pass"
        continue
    fi
    decision="$(check4_decide "$n" 0 0 "" "1:1:1" 1)"
    case " $decision " in *' clear '*) ;; *) continue ;; esac
    lc_unhold "$id" poison sentinel || true
    progress "CHECK4 $id: stale poison cleared — $n attempt(s), below threshold $POISON_AT"
done < <(lc_list_held poison 2>/dev/null)

# ======================================================================================
# CHECK 5 — closed but not landed. One invariant: a closed work bead must carry a LANDED
# landstate record whose tip is an ancestor of the base it lands on. A bead closed without
# landing unblocks its dependents on a lie, and everything downstream then builds on work
# that is not there (law-closed-is-not-landed).
#
# THE INVARIANT HOLDS BY CONSTRUCTION NOW, NOT BY AUDIT. A builder's own close of a work
# bead is converted back to open carrying SPIRA_SUBMITTED_LABEL instead of staying closed
# (aeon.sh, at session teardown), and only bead_close_on_land (lib.sh) — called from the
# landing pass at the moment a commit reaches the base — ever closes one for real. A
# violation here is therefore not "reopen and try again": the aeon that would
# retry did nothing wrong. It is a bug in the landing pass, and reading its log is Ops's
# job, so this reports rather than reopens — reopening a bead whose work IS on the base
# under a fact this check cannot see would throw away finished work for nothing.
#
# NOT CHECKED: superseded (bd supersede records the relation; the work lands under the
# successor's name), spira-dropped (the operator's verdict that no branch is ever coming),
# content-landed (the Sending's own verdict that a branch's diff is already on the base,
# reaped by content rather than by a naming commit — sending.sh labels it but writes no
# landstate record, so this invariant cannot ask for one), close_reason matching
# SUBSUMED/DUPLICATE/tracked in epic (the Concierge's own verdict that the work is retired
# under another bead's name, same as superseded but recorded in prose rather than a
# dependency edge), and any delivers:TYPE label.
# NOT CHECKED AT ALL: non-code types. Only SPIRA_WORK_CLOSE_TYPES
# beads go through the submitted/landed pipeline this invariant polices — spike, ask,
# insight, investigation, event, chore and epic close by the agent's own hand, same as
# always. delivers: is the same shape for a different reason: groom-trigger.sh,
# maechen-trigger.sh and incident.sh file task/bug beads that close on a note, an action
# taken, or child beads filed — never a commit, never a repo: label, never a queue claim.
# aeon.sh's own teardown exempts them from the submitted conversion for the same reason
# (bead_is_work_type call site); this invariant must agree, or it files a false Ops
# incident against every one of them, forever, since none will ever get a LANDED record.
#
# LANDED IS ALSO PROVEN BY THE COMMIT GRAPH. The landing pass prunes landstate records after
# a landing, so a bead that landed rounds ago has no file at all; reading that absence as "not
# landed" filed 138 incidents in 30 minutes on the day this invariant went live and pushed
# the pass past its TimeoutStartSec. The base's own history is the other proof, by the same
# two subject shapes landed() (lib.sh) trusts: the queue's merge subject, "spira: land
# <id>" optionally followed by " — <title>" (land_subject(), lib.sh), or an aeon's own
# "<id>: ..." commit — never a mention elsewhere in a subject, and never a longer id that
# merely starts with this one. The subject list is read
# ONCE per repository per pass into a set (_c5_landed), so each bead costs a lookup, not a
# git walk.
#
# LANDED IS ALSO PROVEN BY THE BEAD'S OWN BRANCH, when neither of the above catches it: a
# commit that reaches the base under a subject naming nobody, with landstate never written
# (sp-noa5s). The subject walk and the landstate file are both readings OF a landing; this
# resolver asks the repository directly — the branch: label's ref, if the reap that runs
# alongside a landing close has not yet claimed it, is checked for ancestry the same way
# the landstate tip is. A bead with no resolvable branch is left to the next resolver,
# never read as unlanded on that account alone.
#
# A FLOOD IS IMPOSSIBLE. At most SPIRA_CHECK5_MAX_FILE incidents are filed per pass (each
# costs an incident.sh run); the rest are counted and named in one log line, and the next
# pass files the next batch. Filing is the rare path and must stay bounded however wrong the
# evidence above turns out to be.
#
# THE CAP BOUNDS THE COUNT, NOT THE TIME — a store having a slow day can make even a
# capped number of incident.sh calls cost minutes (sp-dxntp). SPIRA_CHECK5_BUDGET_SECS
# bounds elapsed wall-clock the same way: whichever limit is hit first stops filing and
# resolving, and the rest wait for the next pass.
#
# Skipped when SPIRA_SKIP_CLOSED_CHECK=1, same reason as before: a fixture that seeds all
# beads directly has none of them in $SPIRA_RUN/<id>.log, so every row is skipped anyway —
# but the query cost per partition is not worth paying to prove that.
# ======================================================================================
_phase "CHECK5"
if [ "${SPIRA_SKIP_CLOSED_CHECK:-0}" != 1 ]; then
_c5_absent_repos=""
declare -A _c5_landed=()
_c5_filed=0; _c5_capped=0; _c5_capped_ids=""; _c5_graph=0
_c5_resolved=0; _c5_resolve_capped=0; _c5_resolve_capped_ids=""
_c5_start="$(date +%s)"; _c5_budget="${SPIRA_CHECK5_BUDGET_SECS:-60}"; _c5_budget_hit=0
_c5_max="${SPIRA_CHECK5_MAX_FILE:-5}"
_c5_resolve_max="${SPIRA_CHECK5_MAX_RESOLVE:-50}"

# _c5_resolve <id> <evidence> — closes the open incident THIS CHECK filed for <id>, now that
# the same check has proven <id> landed. Keyed on the identical dedup ref incident.sh's own
# filing used (the "closed-not-landed:<id>" ref), so this finds exactly the bead
# file_one would have deduped onto had it fired again — never a bead some other check filed.
# Filing is bounded so a flood cannot file forever; this must be bounded the same way so a
# large residue cannot be cleared in one pass that blows the service's TimeoutStartSec.
_c5_resolve() {
    local _id="$1" _evidence="$2" _hash _inc_id _err
    _hash="$(printf '%s' "closed-not-landed:$_id" | sha256sum | cut -c1-8)"
    # ONE LOOKUP PER PASS, NOT PER BEAD. This used to run a `bd list` for every closed bead
    # the commit graph proved landed — ~745 of them at ~1s each — which alone pushed the
    # sentinel past its 15-minute TimeoutStartSec (2026-09-26 21:18-21:30Z: 735s after one
    # filing line). _c5_incidents is filled once, below, before the loop.
    _inc_id="${_c5_incidents[$_hash]:-}"
    [ -n "${_inc_id:-}" ] || return 0
    if [ "$_c5_resolved" -ge "$_c5_resolve_max" ] 2>/dev/null; then
        _c5_resolve_capped=$((_c5_resolve_capped + 1))
        _c5_resolve_capped_ids+="${_c5_resolve_capped_ids:+ }$_id"
        return 0
    fi
    if [ $(( $(date +%s) - _c5_start )) -ge "$_c5_budget" ] 2>/dev/null; then
        _c5_budget_hit=1
        _c5_resolve_capped=$((_c5_resolve_capped + 1))
        _c5_resolve_capped_ids+="${_c5_resolve_capped_ids:+ }$_id"
        return 0
    fi
    # --force: an instance incident rolled up under a root-cause parent carries a
    # blocking dependency, and bd refuses to close a blocked issue. The evidence for
    # this close is the independent landing proof in $_evidence, not the blocker's
    # state, so forcing past it is correct — without --force this silently resolved
    # nothing and left a growing pile of closed-not-landed incidents open.
    if _err="$(bdq close --force "$_inc_id" --reason-file - <<< "$_evidence" 2>&1 1>/dev/null)"; then
        _c5_resolved=$((_c5_resolved + 1))
        log "CHECK5 resolve $_id -> $_inc_id: $_evidence"
    else
        log "CHECK5 resolve $_id -> $_inc_id FAILED: ${_err:-bd close exited nonzero with no message}"
    fi
}
# The open closed-not-landed incidents, keyed by the ref hash incident.sh labels them with,
# read ONCE per pass for _c5_resolve.
declare -A _c5_incidents=()
while read -r _c5_h _c5_i; do
    [ -n "$_c5_h" ] && _c5_incidents["$_c5_h"]="$_c5_i"
done < <(bdjson list --status open,in_progress --limit 0 --label "${SPIRA_INCIDENT_LABEL:-incident}" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for b in (d if isinstance(d, list) else [d]):
    for l in b.get("labels") or []:
        if l.startswith("ref:"): print(l[4:], b["id"])' 2>/dev/null)
while IFS=$'\x1f' read -r id r_name superseded dropped delivers content_landed subsumed branch; do
    [ -n "$id" ] || continue
    # Only beads an aeon worked — anything closed by hand outside the pipeline has its own
    # evidence, and this check has no branch of its own to judge it against.
    [ -f "$SPIRA_RUN/$id.log" ] || continue
    [ "$superseded" = 1 ] && continue
    [ "$dropped" = 1 ] && continue
    [ -n "${delivers:-}" ] && continue
    [ "$content_landed" = 1 ] && continue
    [ "$subsumed" = 1 ] && continue
    r_path="$(repo_root "${r_name:-}")" || {
        case $'\n'"$_c5_absent_repos" in
            *$'\n'"$r_name"$'\n'*) ;;
            *) _c5_absent_repos+="${r_name}"$'\n'
               log "CHECK5: repo:$r_name is not in repo-map — skipping its closed beads" ;;
        esac
        continue; }
    if [ "$r_path" != "${subj_repo:-}" ]; then
        subj_repo="$r_path"
        subj_refs="$(spira_landrefs "$r_path")" || subj_refs=""
        subj_base="${subj_refs%% *}"
        # ONE WALK PER REPOSITORY, into a set of the ids the base's subjects prove landed.
        # Three shapes: the queue's "spira: land <id>", an aeon's "<id>: ...", and a round
        # merge committed under git's default "Merge branch 'spira/<id>' ..." (Concierge rounds
        # before round.sh used the land subject) — without it those closes were re-flagged every
        # pass, and filing them pushed the pass past TimeoutStartSec (sp-dxntp).
        # Rows arrive sorted by repository, so this runs once per repository per pass.
        _c5_landed=()
        if [ -n "${subj_base:-}" ]; then
            while IFS= read -r _c5_lid; do
                [ -n "$_c5_lid" ] && _c5_landed["$_c5_lid"]=1
            done < <(git -C "$r_path" log --format=%s "$subj_base" 2>/dev/null | awk '
                substr($0, 1, 12) == "spira: land " { r = substr($0, 13); split(r, a, " "); if (a[1] != "") print a[1]; next }
                index($0, "Merge branch \047spira/") == 1 { r = substr($0, 21); sub(/\047.*/, "", r); if (r != "" && r !~ / / && r !~ /^round-/) print r; next }
                match($0, /^[^: ]+:/) { print substr($0, 1, RLENGTH - 1) }')
        fi
    fi
    # CANNOT TELL IS NOT "NOT LANDED". A repository whose base cannot be resolved is left
    # unjudged rather than read as unlanded, which would report every closed bead in it.
    if [ -z "${subj_base:-}" ]; then
        log "CHECK5 $id: cannot resolve the ref $r_name lands on — not judging whether it landed"
        continue
    fi
    if [ -n "${_c5_landed[$id]:-}" ]; then
        _c5_graph=$((_c5_graph + 1))
        _c5_resolve "$id" "$id is landed: $r_name's base ($subj_base) names it in a 'spira: land' or '<id>:' subject, proven by the same commit-graph walk that filed this incident (law-closed-is-not-landed)."
        continue
    fi
    # RESOLVER 3 (sp-noa5s) — see the header block above.
    if [ -n "${branch:-}" ] \
           && git -C "$r_path" show-ref --verify -q "refs/heads/$branch" \
           && git -C "$r_path" merge-base --is-ancestor "refs/heads/$branch" "$subj_base" 2>/dev/null; then
        _c5_resolve "$id" "$id is landed: its recorded branch ($branch) tip is an ancestor of $r_name's base ($subj_base), proven directly by the commit graph — no commit subject names it and no landstate record exists (law-closed-is-not-landed)."
        continue
    fi
    _c5_ls_state=""; _c5_ls_tip=""
    if [ -r "$SPIRA_RUN/landstate/$id" ]; then
        read -r _c5_ls_state _c5_ls_tip _ < "$SPIRA_RUN/landstate/$id" 2>/dev/null || true
    fi
    if [ "${_c5_ls_state:-}" = LANDED ] && [ -n "${_c5_ls_tip:-}" ] && [ "$_c5_ls_tip" != none ] \
           && git -C "$r_path" merge-base --is-ancestor "$_c5_ls_tip" "$subj_base" 2>/dev/null; then
        _c5_resolve "$id" "$id is landed: its LANDED landstate tip ($_c5_ls_tip) is an ancestor of $r_name's base ($subj_base)."
        continue
    fi
    if [ "$_c5_filed" -ge "$_c5_max" ] 2>/dev/null; then
        _c5_capped=$((_c5_capped + 1)); _c5_capped_ids+="${_c5_capped_ids:+ }$id"
        continue
    fi
    if [ $(( $(date +%s) - _c5_start )) -ge "$_c5_budget" ] 2>/dev/null; then
        _c5_budget_hit=1
        _c5_capped=$((_c5_capped + 1)); _c5_capped_ids+="${_c5_capped_ids:+ }$id"
        continue
    fi
    _c5_filed=$((_c5_filed + 1))
    log "CHECK5 $id: closed with no LANDED record on $r_name ($subj_base) — filing an Ops incident"
    SPIRA_DB="$SPIRA_DB" \
    SPIRA_INCIDENT_TYPE=bug \
    SPIRA_INCIDENT_PRIORITY=1 \
    SPIRA_INCIDENT_ACTOR=sentinel \
    SPIRA_INCIDENT_LABELS="${SPIRA_SCOPE_LABEL:-spira},${SPIRA_INCIDENT_LABEL:-incident}" \
    SPIRA_INCIDENT_REPO="${SPIRA_SCOPE_LABEL:-spira}" \
    SPIRA_INCIDENT_REF="closed-not-landed:$id" \
    SPIRA_INCIDENT_CAUSE=closed-not-landed \
    bash "${SPIRA_INCIDENT_SH:-$SPIRA_HOME/incident.sh}" file \
        "CLOSED NOT LANDED: $id has no LANDED record on $r_name" \
        - <<< "landstate=${_c5_ls_state:-none} tip=${_c5_ls_tip:-none} base=$subj_base repo=$r_name. The landing pass closes work beads when their commit reaches the base (bead_close_on_land, lib.sh); this bead is closed with no such record. Check the landing pass's own log before assuming the work is missing." \
        >/dev/null 2>&1 || true
done < <(
    home_repo="$(spira_home_repo)"
    while IFS=$'\t' read -r part _; do
        [ -n "$part" ] || continue
        bdjson list --status closed --limit 0 --label "$part" 2>/dev/null | python3 -c '
import sys, json, re
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
home = sys.argv[1]
work_types = set(sys.argv[2].split())
for i in (d if isinstance(d, list) else [d]):
    if (i.get("issue_type") or "") not in work_types:
        continue
    repo = next((l[5:] for l in (i.get("labels") or []) if l.startswith("repo:")), home)
    # `bd list` AND `bd show` NAME THE SAME FIELD DIFFERENTLY. show returns
    # {"dependency_type": "supersedes"}; list returns {"type": "supersedes"}. Accept
    # either spelling rather than the one the neighbouring command happened to use.
    sup = 1 if any((x.get("dependency_type") or x.get("type")) == "supersedes"
                   for x in (i.get("dependencies") or [])) else 0
    drop = 1 if "spira-dropped" in (i.get("labels") or []) else 0
    deliv = "1" if any(l.startswith("delivers:") for l in (i.get("labels") or [])) else ""
    cl = 1 if "content-landed" in (i.get("labels") or []) else 0
    reason = (i.get("close_reason") or "")
    subs = 1 if re.search(r"SUBSUMED|DUPLICATE|tracked in epic", reason, re.I) else 0
    branch = next((l[7:] for l in (i.get("labels") or []) if l.startswith("branch:")), "")
    print("\x1f".join([i["id"], repo, str(sup), str(drop), deliv, str(cl), str(subs), branch]))' "$home_repo" \
            "${SPIRA_WORK_CLOSE_TYPES:-task bug feature}" 2>/dev/null
    done <<< "$PARTITIONS" |
    sort -u -t$'\x1f' -k2,2 -k1,1
)
[ -n "$PARTITIONS" ] || log "CHECK5 no persona in the chamber declares a partition — no closed bead is being checked for landing"
[ "$_c5_graph" -gt 0 ] && log "CHECK5: $_c5_graph closed bead(s) with no LANDED record proven landed by the base's commit graph"
if [ "$_c5_capped" -gt 0 ]; then
    log "CHECK5: filed $_c5_filed incident(s), the cap (SPIRA_CHECK5_MAX_FILE=$_c5_max); $_c5_capped more not filed this pass: $_c5_capped_ids"
fi
[ "$_c5_resolved" -gt 0 ] && log "CHECK5: resolved $_c5_resolved incident(s) for beads this pass proved landed"
if [ "$_c5_resolve_capped" -gt 0 ]; then
    log "CHECK5: resolved $_c5_resolved incident(s), the cap (SPIRA_CHECK5_MAX_RESOLVE=$_c5_resolve_max); $_c5_resolve_capped more proven landed but not resolved this pass: $_c5_resolve_capped_ids"
fi
[ "$_c5_budget_hit" -eq 1 ] && log "CHECK5: pass budget exhausted (SPIRA_CHECK5_BUDGET_SECS=$_c5_budget) — remaining filings/resolves deferred to the next pass"
unset -f _c5_resolve
unset _c5_landed _c5_filed _c5_capped _c5_capped_ids _c5_graph _c5_max _c5_lid
unset _c5_resolved _c5_resolve_capped _c5_resolve_capped_ids _c5_resolve_max
unset _c5_start _c5_budget _c5_budget_hit
fi  # SPIRA_SKIP_CLOSED_CHECK
fi  # AUDIT (CHECK 4, CHECK 5)

# ======================================================================================
# CHECK 4/5/6b/7c/7d DISPATCH — start the audit worker and move on. Unit name is the
# mutex (systemd will not start a second `spira-audit`), `--collect` keeps a FAILED unit
# from blocking every later dispatch, and the mailbox/status files are drained the same
# way CHECK 6 drains landing.progress: what an async worker did is only known from what
# it wrote, read back on a LATER pass.
# ======================================================================================
if [ "$AUDIT" = 0 ]; then
AUDIT_UNIT="${SPIRA_AUDIT_UNIT:-spira-audit}"
AUDIT_MAXSEC="${SPIRA_AUDIT_MAXSEC:-1800}"
AUDIT_STALE="${SPIRA_AUDIT_STALE:-1800}"
AUDIT_STATUS="$SPIRA_RUN/audit.status"

audit_active() {
    [ "$("${SPIRA_SYSTEMCTL:-systemctl}" --user is-active "$AUDIT_UNIT.service" 2>/dev/null)" = active ]
}

# DRAIN BY RENAME — see land_drain for why: a line is counted exactly once even when a
# worker dies mid-drain or between passes.
audit_drain() {
    local mine="$SPIRA_RUN/audit.progress.drain.$$" f line
    [ -s "$AUDIT_MAILBOX" ] && mv -f "$AUDIT_MAILBOX" "$mine" 2>/dev/null
    for f in "$SPIRA_RUN"/audit.progress.drain.*; do
        [ -f "$f" ] || continue
        while IFS= read -r line; do
            [ -n "$line" ] || continue
            progress "$line"
        done < "$f"
        rm -f "$f"
    done
}

audit_drain

audit_age=-1
if [ -r "$AUDIT_STATUS" ]; then
    # shellcheck disable=SC1090
    eval "$(sed -n 's/^\(SP_AUDIT_[A-Z]*\)=\([0-9-]*\)$/\1=\2/p' "$AUDIT_STATUS" 2>/dev/null)"
    audit_age=$(( $(date +%s) - ${SP_AUDIT_AT:-0} ))
    log "CHECK4/5 audit: last run ${audit_age}s ago — rc=${SP_AUDIT_RC:-?}"
fi

if audit_active; then
    log "CHECK4/5 audit: already running — this pass does not start another"
elif "${SPIRA_LAUNCH:-systemd-run}" --user --collect --quiet \
        --unit="$AUDIT_UNIT" \
        --property=RuntimeMaxSec="$AUDIT_MAXSEC" \
        --property=CPUQuota="${SPIRA_AUDIT_CPU_QUOTA:-40}%" --property=Nice=10 \
        --property=StandardOutput="append:$SPIRA_RUN/audit.log" \
        --property=StandardError="append:$SPIRA_RUN/audit.log" \
        --setenv=PATH="$PATH" --setenv=HOME="$HOME" \
        --setenv=SPIRA_HOME="$SPIRA_HOME" --setenv=SPIRA_RUN="$SPIRA_RUN" \
        --setenv=SPIRA_DB="$SPIRA_DB" --setenv=SPIRA_REPO="${SPIRA_REPO:-}" \
        --setenv=SPIRA_REPO_MAP="$SPIRA_REPO_MAP" \
        --setenv=SPIRA_HOME_REPO="$(spira_home_repo)" \
        --setenv=SPIRA_BD="${SPIRA_BD:-bd}" --setenv=SPIRA_GH="${SPIRA_GH:-gh}" \
        --setenv=SPIRA_POISON_AT="$POISON_AT" --setenv=SPIRA_REQUEUE_AT="$REQUEUE_AT" \
        --setenv=SPIRA_RECLAIM_AT="$RECLAIM_AT" \
        --setenv=SPIRA_ASK_LABEL="${SPIRA_ASK_LABEL:-}" \
        --setenv=SPIRA_SCOPE_LABEL="${SPIRA_SCOPE_LABEL:-}" \
        --setenv=SPIRA_WORK_CLOSE_TYPES="${SPIRA_WORK_CLOSE_TYPES:-}" \
        "$SPIRA_HOME/sentinel.sh" --audit 2>/dev/null
then
    log "CHECK4/5 audit: dispatched as $AUDIT_UNIT"
    [ -f "$SPIRA_RUN/audit.dispatched" ] || date +%s > "$SPIRA_RUN/audit.dispatched"
elif audit_active; then
    log "CHECK4/5 audit: started underneath this pass — not starting another"
else
    log "CHECK4/5 audit WARN: could not dispatch the audit worker; poison, closed-not-landed, sending and collision checks will not run until this is fixed"
fi

if [ "$audit_age" -lt 0 ] && [ -f "$SPIRA_RUN/audit.dispatched" ]; then
    audit_age=$(( $(date +%s) - $(cat "$SPIRA_RUN/audit.dispatched" 2>/dev/null || date +%s) ))
fi
if ! audit_active && [ "$audit_age" -gt "$AUDIT_STALE" ]; then
    log "CHECK4/5 audit WARN: no audit pass has completed in ${audit_age}s and none is running"
fi

# READ THE MAILBOX AGAIN — an audit run that finished while this dispatch was happening
# has movements to report now rather than at the next tick.
audit_drain
fi  # AUDIT (dispatch)

# EVERYTHING FROM HERE THROUGH CHECK 7's FILL LOOP IS THE NORMAL PASS ONLY — land-dispatch
# and summon are its job; the audit worker (started above) must not also try to land
# branches or summon aeons under its own PID.
if [ "$AUDIT" = 0 ]; then
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
# SPIRA_LAND_CPU_QUOTA AND A RuntimeMaxSec, BECAUSE THIS LEAVES THE SENTINEL'S CGROUP. A
# transient unit is its own cgroup, so without a quota of its own the split from
# spira-sentinel.service would quietly hand the box more Spira than it had before. This
# machine also runs prod, two CI runners and the operator's session, and anything that polls
# in a loop on it gets a quota before it is enabled (law-fence-loops-on-shared-hardware).
# RuntimeMaxSec is the knob that applies to a `simple` service, which is what systemd-run
# creates; TimeoutStartSec would be ignored.
# ======================================================================================
_phase "CHECK6"
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

# DISABLED ESSENTIAL TIMER CHECK — escalate a TIMER_PRIORITY timer (sentinel, ops,
# watchtower, archivist, archive, skew) that is disabled with no recorded ctrl suspension
# while the world is running. world.sh start catches this at the moment of a restart;
# this is the periodic leg for everything that happens between one start and the next.
[ -r "${SPIRA_HOME}/watchtower.sh" ] && \
    bash "${SPIRA_HOME}/watchtower.sh" --disabled-timer-check 2>/dev/null || true

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
        --property=CPUQuota="${SPIRA_LAND_CPU_QUOTA:-70}%" --property=Nice=10 \
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
# Same-repo work-bead blockers release earlier under the stacked-dependents rule
# (stack_max_depth) — see wiki/projects/spira/designs/stacked-dependents-2026-09-28.md.
_phase "CHECK3b"
mark_queue_waiters 2>/dev/null || true
close_landed_queue_waiters 2>/dev/null || true

# CHECK 3c — a coordination bead with any open parent-child-linked child is not currently
# dispatchable: mark_open_children applies SPIRA_OPEN_CHILDREN_LABEL so fayth_ready and the
# claim it counts on both exclude it, and removes the label once every child has closed.
mark_open_children 2>/dev/null || true

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
# ck7_summon_pass lives in lib.sh, under one flock shared with sentinel.sh --summon-only
# (sp-0y2av), so the two entry points can never both summon into the same freed slot — and
# so it can be exercised by test-fayth.sh, which a version inlined here could not be:
# everything else in a sentinel pass touches the real repository and the real database.
# ======================================================================================
_phase "CHECK7"
ck7_summon_pass
fi  # AUDIT (CHECK 6, CHECK 7)

# CHECK 6b, 7c AND 7d NOW RUN IN THE AUDIT WORKER (sp-994y9), not here — see the dispatch
# above. They used to sit in this normal pass, after CHECK 7, for the same reason CHECK 6b
# gives below: summon first, reap after. That reorder only ever saved the CURRENT pass's
# summon; it could not stop the walk from making the WHOLE pass, and therefore the NEXT
# tick's summon, wait behind it. Only running the walk in a separate process does that.
if [ "$AUDIT" = 1 ]; then
# CHECK 6b — the Sending. Send the branch and worktree of every bead whose work is now an
# ancestor of its repository's base.
#
# sending.sh judges by ancestry alone, never by bead status, so it cannot be talked into
# deleting work by a database that is merely optimistic.
#
# --skip-queue: A QUEUE-MODE REPO IS NOT WALKED HERE AT ALL (sp-jci6o). Its landed batch
# members are reaped at landing by bead_close_on_land (spira_reap_landed_branch, lib.sh) —
# the queue's own verdict already knows exactly which branches just landed, so re-scanning
# every spira/* branch in that repo every two minutes to rediscover the same fact by ancestry
# is the cost this flag removes. A daily straggler sweep (--queue-only, its own timer) is
# what catches whatever the landing-time reap missed.
#
# BASE-UNCHANGED SKIP. When no SWEPT repo's land ref has moved since the last walk, nothing
# could have landed. Stamp: per-repo name=sha lines at $SPIRA_RUN/sending.base, written after
# each full walk. Repos that repo_root cannot resolve, or that are in queue mode, are skipped
# in both directions — a queue-mode repo's base moving must not force a walk of every other
# repository just to learn again that this one is not swept here.
# ======================================================================================
_phase "CHECK6b"
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
            repo_land_queued "$_sr_name" && continue
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
    sent="$("$SPIRA_HOME/sending.sh" --skip-queue 2>&1)"
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
        repo_land_queued "$_sr_name" && continue
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
_phase "CHECK7c"
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
# is mechanical whenever the squatter's own bead is closed, clean and unheld: free that
# worktree and let the blocked bead through. Only a squatter that is open, dirty or still
# live needs the operator, and that is what still gets parked with $SPIRA_ASK_LABEL.
# ======================================================================================
_phase "CHECK7d"
if [ "${SPIRA_SKIP_RECLAIM:-0}" != 1 ]; then
collision_out="$(detect_branch_collisions 2>/dev/null)"
if [ -n "$collision_out" ]; then
    printf '%s\n' "$collision_out"
    n_col="$(grep -c '^COLLISION' <<< "$collision_out" || true)"
    park_out="$(park_branch_collisions "$collision_out")"
    [ -n "$park_out" ] && printf '%s\n' "$park_out"
    n_freed="$(grep -c '^FREED' <<< "$park_out" || true)"
    n_unlabeled="$(grep -c '^UNLABELED' <<< "$park_out" || true)"
    n_parked=$((n_col - n_freed - n_unlabeled))
    if [ "$n_freed" -gt 0 ]; then
        log "CHECK7d: freed $n_freed stale squatting worktree(s) whose owning bead is closed and clean"
        act "freed $n_freed branch-collision worktree(s)"
    fi
    if [ "$n_unlabeled" -gt 0 ]; then
        log "CHECK7d: cut $n_unlabeled bead(s) off an inherited branch: label naming another bead's canonical branch"
        act "unlabeled $n_unlabeled inherited branch-collision bead(s)"
    fi
    if [ "$n_parked" -gt 0 ]; then
        log "CHECK7d: $n_parked bead(s) whose recorded branch is held by another bead's worktree — parking with $SPIRA_ASK_LABEL"
        act "parked $n_parked branch-collision bead(s)"
    fi
fi
fi  # SPIRA_SKIP_RECLAIM
fi  # AUDIT (CHECK 6b, 7c, 7d)

if [ "$AUDIT" = 1 ]; then
    # THE STATUS FILE IS THE POSITIVE CONTROL the next normal pass reads (see the dispatch
    # block above) — without it, "the audit worker has never once completed" and "it just
    # finished" are indistinguishable from outside this process.
    printf 'SP_AUDIT_AT=%s\nSP_AUDIT_RC=0\n' "$(date +%s)" > "$SPIRA_RUN/audit.status"
    log "audit pass complete — $acted action(s), $progressed progress"
    exit 0
fi

if [ "$GOAL_REACHED" = 1 ]; then
    _phase "end"
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
_phase "CHECK8"
_ck8_now="$(date +%s)"
_ck8_last=0; [ -f "$COOLDOWN" ] && _ck8_last="$(cat "$COOLDOWN" 2>/dev/null || echo 0)"
case "$(check8_should_judge "$plan_ready" "$plan_inprog" "$n_open" "$progressed" \
            "$_ck8_last" "$_ck8_now" "$INFERENCE_EVERY")" in
    cooldown)
        log "starved, but inference is in cooldown ($(( INFERENCE_EVERY - _ck8_now + _ck8_last ))s left)"
        _phase "end"
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

_phase "end"
log "pass complete — $acted action(s), $progressed progress"
