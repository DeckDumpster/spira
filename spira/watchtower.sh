#!/usr/bin/env bash
#
# watchtower.sh — wake Ops on a timer and hand it the state of the pipeline.
#
#   watchtower.sh            gather, and file the sweep Ops claims
#   watchtower.sh --show     gather and print; touch nothing
#
# WHY THIS EXISTS (law-detection-outranks-rejection). Ops has always been able to hear about
# a unit that CRASHED — incident.sh files from a systemd OnFailure, and that is its only
# intake. A queue that has stopped moving while every unit is happily `active` is invisible
# to it, and that is not a hypothetical: on 2026-09-07 nothing landed for the better part of
# a morning while every process was green, every log line was individually true, and eleven
# consecutive gate runs correctly reported that the next pass would take it. Nothing noticed.
# The operator did, by looking.
#
# So this is the second intake: not "a process died" but "the pipeline is not doing the thing
# it exists to do". What the system IS, stated plainly, is N workers pulling from a DAG into
# a merge queue — so the numbers that matter are the depth of that queue, how long its oldest
# member has waited, and how long it has been since anything came out of the far end.
#
# IT DOES NOT DECIDE (the operator's call, 2026-09-07, over a threshold-driven detector).
# This program gathers and hands over; an Ops aeon reads the snapshot and decides what is
# wrong and what beads to cut. Thresholds anticipate only the outage you already had — every
# stall so far has been a shape nobody had a number for.
#
# THE SESSION IS BOUNDED BY THIS CADENCE, and that constraint lives in ops.fayth: a sweep
# arrives every ten minutes, so an Ops session gets eight. The first live one ran eighteen
# and would have been working the 22:30 snapshot while 22:40 and 22:50 queued behind it. The
# sweep is also deduped to one open at a time, so a cycle arriving while Ops is still working
# the last one bumps a recurrence rather than filing a second.
#
# IT IS DETERMINISTIC AND CHEAP, deliberately (law-deterministic-before-inference). It reads
# files that already exist and shells out to nothing slow, because the one thing a watchtower
# may not be is another thing that is down during an outage.
#
# A FIELD IT COULD NOT READ RENDERS `?`, NEVER 0 (law-absence-needs-a-positive-control). A
# broken probe reporting "0 branches waiting" is an all-clear that displaces the suspicion
# which would have prompted a look — the exact failure this whole program is a response to.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

# All six queue-stall detectors (deadlock, attribution-failed, sort-failed, loop-stalled,
# ci-stalled, starved) plus ci-red now live in czar.sh --pass (sp-rpibz), running on a
# 30s timer with deterministic remedies. The --queue-checks handler has been removed.

if [ "${1:-}" = "--queue-checks" ]; then
    log "watchtower: --queue-checks is retired; detectors now run in czar.sh --pass (sp-rpibz)"
    exit 0
fi

# --throttle-check: admission gate for the task pool, called by sentinel.sh on every pass.
# Two inputs, two outputs:
#   depth >= DEPTH_AT AND drain active  → write stamp (throttle engaged, pool held at 0)
#   depth >= DEPTH_AT AND drain zero    → escalate WITHOUT writing stamp (stall, not capacity)
# Throttling a stalled queue delays repairs rather than reducing load — wrong answer every time.
# STAMP: $SPIRA_RUN/queue-throttled — sentinel.sh CHECK7 reads it before summing task fayths.
# POSITIVE CONTROL: depth and drain are computed from landstate files, same source cockpit uses.
if [ "${1:-}" = "--throttle-check" ]; then
    [ -f "$SPIRA_RUN/world.halted" ] && {
        log "watchtower: throttle-check skipped — world is halted"
        exit 0
    }

    _tc_stamp="${SPIRA_THROTTLE_STAMP:-$SPIRA_RUN/queue-throttled}"
    _tc_override="${SPIRA_QUEUE_THROTTLE_OVERRIDE:-}"
    _tc_inc="${SPIRA_INCIDENT_SH:-$(dirname "$0")/incident.sh}"
    _tc_depth_at="${SPIRA_QUEUE_THROTTLE_DEPTH_AT:-16}"
    _tc_release_at="${SPIRA_QUEUE_THROTTLE_RELEASE_AT:-8}"
    _tc_stall_mins="${SPIRA_QUEUE_THROTTLE_STALL_MINS:-50}"

    if [ "$_tc_override" = "off" ]; then
        rm -f "$_tc_stamp"
        log "watchtower: throttle-check — override=off, admission not throttled"
        exit 0
    fi

    # Depth: CERTIFIED records whose branch still exists AND tip not yet on the land ref.
    # Stale records (branch gone or tip already merged) inflate depth and can hold the pool
    # past the throttle threshold when no work is actually waiting.
    # SPIRA_TC_REPO and SPIRA_TC_LAND_REF are seams for test isolation (cf. SPIRA_INCIDENT_SH).
    _tc_repo="${SPIRA_TC_REPO-${SPIRA_REPO:-}}"
    _tc_lref="${SPIRA_TC_LAND_REF:-}"
    if [ -n "$_tc_repo" ] && [ -z "$_tc_lref" ]; then
        _tc_lref="$(spira_landref "$_tc_repo" 2>/dev/null)" || true
    fi
    _tc_depth=0
    if [ -d "$SPIRA_RUN/landstate" ]; then
        while IFS= read -r _tc_lsf; do
            [ -r "$_tc_lsf" ] || continue
            _tc_st=""; _tc_tip=""
            read -r _tc_st _tc_tip _ < "$_tc_lsf" 2>/dev/null || true
            [ "$_tc_st" = "CERTIFIED" ] || continue
            # Filter stale records: branch must exist AND tip not yet merged.
            # Use auto-detection for repo path when SPIRA_TC_REPO is unset (production case).
            _tc_id="$(basename "$_tc_lsf")"
            if [ -n "$_tc_repo" ]; then
                git -C "$_tc_repo" rev-parse --verify --quiet \
                    "refs/heads/spira/$_tc_id" >/dev/null 2>&1 || continue
            else
                git rev-parse --verify --quiet \
                    "refs/heads/spira/$_tc_id" >/dev/null 2>&1 || continue
            fi
            if [ -n "$_tc_lref" ] && [ -n "$_tc_tip" ] && [ "$_tc_tip" != "none" ]; then
                if [ -n "$_tc_repo" ]; then
                    git -C "$_tc_repo" merge-base --is-ancestor \
                        "$_tc_tip" "$_tc_lref" 2>/dev/null && continue
                else
                    git merge-base --is-ancestor \
                        "$_tc_tip" "$_tc_lref" 2>/dev/null && continue
                fi
            fi
            _tc_depth=$(( _tc_depth + 1 ))
        done < <(find "$SPIRA_RUN/landstate" -maxdepth 1 -type f 2>/dev/null)
    fi

    # Drain: minutes since the most recent LANDED landstate record.
    # "?" means no LANDED record could be read — treated as stall (drain unknown = drain zero).
    _tc_now_s="$(date +%s)"
    _tc_last_landed="?"
    if [ -d "$SPIRA_RUN/landstate" ]; then
        while IFS= read -r _tc_lf; do
            [ -r "$_tc_lf" ] || continue
            _tc_lst=""; _tc_lat=""
            read -r _tc_lst _ _tc_lat _ < "$_tc_lf" 2>/dev/null || true
            [ "$_tc_lst" = "LANDED" ] || continue
            case "$_tc_lat" in ''|*[!0-9]*) continue ;; esac
            if [ "$_tc_last_landed" = "?" ] || [ "$_tc_lat" -gt "$_tc_last_landed" ]; then
                _tc_last_landed="$_tc_lat"
            fi
        done < <(find "$SPIRA_RUN/landstate" -maxdepth 1 -type f 2>/dev/null)
    fi
    _tc_since_land="?"
    [ "$_tc_last_landed" != "?" ] && \
        _tc_since_land=$(( (_tc_now_s - _tc_last_landed) / 60 ))

    _tc_throttled=0; [ -f "$_tc_stamp" ] && _tc_throttled=1

    # Drain is "active" when since_land is numeric and below the stall threshold.
    # A "?" or stall-length silence treats the queue as stalled: escalate, do not throttle.
    _tc_drain_ok=0
    if [ "$_tc_since_land" != "?" ] && \
       [ "$_tc_since_land" -lt "$_tc_stall_mins" ] 2>/dev/null; then
        _tc_drain_ok=1
    fi

    if [ "$_tc_depth" -ge "$_tc_depth_at" ] 2>/dev/null; then
        if [ "$_tc_drain_ok" = "1" ]; then
            if [ "$_tc_throttled" = "0" ]; then
                # ENGAGE: depth high, drain active.
                printf 'since=%s depth=%s since_land=%sm\n' \
                    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$_tc_depth" "$_tc_since_land" \
                    > "$_tc_stamp"
                log "watchtower: throttle engaged — depth=${_tc_depth}>=${_tc_depth_at}, since_land=${_tc_since_land}m"
                [ -r "$_tc_inc" ] && \
                    printf 'Queue admission throttled: CERTIFIED depth %s (threshold: %s branches), last landing %sm ago.\n\nBuilders are held; Ops, groomer, and other lanes continue.\n\nEngages when depth >= %s AND drain active (< %sm since landing).\nLifts when depth < %s.\nOverride: SPIRA_QUEUE_THROTTLE_OVERRIDE=off in spira.conf\n' \
                        "$_tc_depth" "$_tc_depth_at" "$_tc_since_land" \
                        "$_tc_depth_at" "$_tc_stall_mins" "$_tc_release_at" | \
                    SPIRA_DB="$SPIRA_DB" \
                    SPIRA_INCIDENT_TYPE=task \
                    SPIRA_INCIDENT_PRIORITY=2 \
                    SPIRA_INCIDENT_ACTOR=watchtower \
                    SPIRA_SIN_EXEMPT=1 \
                    SPIRA_INCIDENT_REPO=spira \
                    SPIRA_INCIDENT_REF=incident:queue-throttle-engaged \
                    SPIRA_INCIDENT_CAUSE=throttle-engaged \
                    bash "$_tc_inc" file \
                        "QUEUE THROTTLED: depth ${_tc_depth}, drain ${_tc_since_land}m since landing" \
                        - >/dev/null || true
                log "watchtower: throttle-engage escalation filed"
            else
                log "watchtower: throttle-check — still throttled (depth=${_tc_depth}>=${_tc_depth_at})"
            fi
        else
            # Drain is zero: FAULT or deliberate wait state (async gate pending). Do not throttle.
            # Check if the stall is deliberate (async gate not yet on main per sop-sp-hsxk8 CHECK).
            _tc_async_on_main=0
            if [ -n "$_tc_repo" ]; then
                _tc_async_on_main="$(git -C "$_tc_repo" log --oneline "${_tc_lref:-origin/main}" 2>/dev/null | grep -E '(sp-c8w16|sp-74gwk)' | wc -l)" || _tc_async_on_main=0
            else
                _tc_async_on_main="$(git log --oneline origin/main 2>/dev/null | grep -E '(sp-c8w16|sp-74gwk)' | wc -l)" || _tc_async_on_main=0
            fi
            _tc_stall_is_deliberate=0
            [ "$_tc_async_on_main" = "2" ] || _tc_stall_is_deliberate=1

            if [ "$_tc_stall_is_deliberate" = "1" ]; then
                # Stall is deliberate: async gate implementation (sp-c8w16/sp-74gwk) not yet on main.
                # Do not escalate. Log it and continue.
                log "watchtower: throttle-check — depth=${_tc_depth}>=${_tc_depth_at} but stall is deliberate (async gate not on main) — not escalating"
            else
                # Stall is real: both async gate commits are on main, but queue is still stalled. FAULT.
                log "watchtower: throttle-check — depth=${_tc_depth}>=${_tc_depth_at} but since_land=${_tc_since_land}m>=${_tc_stall_mins}m stall — not throttling (fault)"
                [ "$_tc_throttled" = "0" ] && [ -r "$_tc_inc" ] && \
                    printf 'Queue depth %s above throttle threshold (%s) but drain has been zero for %sm (stall threshold: %sm).\n\nThis is a QUEUE FAULT, not a capacity condition. Throttling builders delays repairs.\nInvestigate: landing loop, batch CI, gate status.\n' \
                        "$_tc_depth" "$_tc_depth_at" "${_tc_since_land:-?}" "$_tc_stall_mins" | \
                    SPIRA_DB="$SPIRA_DB" \
                    SPIRA_INCIDENT_TYPE=task \
                    SPIRA_INCIDENT_PRIORITY=1 \
                    SPIRA_INCIDENT_ACTOR=watchtower \
                    SPIRA_SIN_EXEMPT=1 \
                    SPIRA_INCIDENT_REPO=spira \
                    SPIRA_INCIDENT_REF=incident:queue-throttle-stall \
                    SPIRA_INCIDENT_CAUSE=throttle-stall \
                    bash "$_tc_inc" file \
                        "QUEUE: deep+stalled (depth ${_tc_depth}, no landings for ${_tc_since_land:-?}m)" \
                        - >/dev/null || true
            fi
        fi
    elif [ "$_tc_throttled" = "1" ] && [ "$_tc_depth" -lt "$_tc_release_at" ] 2>/dev/null; then
        # LIFT: depth below release threshold.
        rm -f "$_tc_stamp"
        log "watchtower: throttle lifted — depth=${_tc_depth}<${_tc_release_at}"
        [ -r "$_tc_inc" ] && \
            printf 'Queue throttle lifted: CERTIFIED depth now %s (below release threshold %s).\n\nBuilder admission is no longer throttled.\n' \
                "$_tc_depth" "$_tc_release_at" | \
            SPIRA_DB="$SPIRA_DB" \
            SPIRA_INCIDENT_TYPE=task \
            SPIRA_INCIDENT_PRIORITY=2 \
            SPIRA_INCIDENT_ACTOR=watchtower \
            SPIRA_SIN_EXEMPT=1 \
            SPIRA_INCIDENT_REPO=spira \
            SPIRA_INCIDENT_REF=incident:queue-throttle-lifted \
            SPIRA_INCIDENT_CAUSE=throttle-lifted \
            bash "$_tc_inc" file \
                "QUEUE THROTTLE LIFTED: depth ${_tc_depth}" \
                - >/dev/null || true
        log "watchtower: throttle-lift escalation filed"
    else
        log "watchtower: throttle-check — $([ "$_tc_throttled" = "1" ] && echo "throttled" || echo "clear") (depth=${_tc_depth} since_land=${_tc_since_land}m)"
    fi

    exit 0
fi

# --czar-outcome-check: verify that czar-trigger beads are being handled and that the
# conditions they were fired for have cleared after closure. Called by sentinel.sh on every
# pass (2-minute cadence), the same hook as --queue-checks and --throttle-check.
#
# TWO CHECKS:
# 1. UNCLAIMED — a czar-trigger bead open for more than SPIRA_CZAR_UNCLAIMED_MINS without
#    being claimed or closed. The czar's summoning budget is 5 minutes; this window is
#    wider to absorb sentinel cadence and rate-limit pauses.
# 2. NOT CLEARED — a czar-trigger bead that was closed but the same condition returned:
#    a newer bead with the same external_ref was filed after the closed bead's close time,
#    and the closed bead was itself closed more than SPIRA_CZAR_OUTCOME_MINS ago.
#    This is law-measure-the-outcome: the czar closing a bead is not evidence the condition
#    cleared; the absence of a subsequent bead for the same class is.
#
# DEDUPED PER BEAD via SPIRA_INCIDENT_REF. Each unclaimed or not-cleared escalation carries
# a bead-scoped ref so a bead that fires the check on two consecutive passes does not produce
# two escalations. incident.sh's lookback window handles the dedup.
#
# USES SPIRA_BD for bd queries. This makes it the first watchtower subcommand with a database
# dependency; the others are file-reads-only. The embedded Dolt engine makes each query ~50ms,
# which is acceptable on the sentinel's 2-minute cadence.
if [ "${1:-}" = "--czar-outcome-check" ]; then
    [ -f "$SPIRA_RUN/world.halted" ] && {
        log "watchtower: czar-outcome-check skipped — world is halted"
        exit 0
    }

    _co_inc="${SPIRA_INCIDENT_SH:-$(dirname "$0")/incident.sh}"
    _co_outcome_mins="${SPIRA_CZAR_OUTCOME_MINS:-30}"
    _co_unclaimed_mins="${SPIRA_CZAR_UNCLAIMED_MINS:-10}"
    _co_label="${SPIRA_CZAR_LABEL:-czar-trigger}"
    _co_now="$(date +%s)"

    [ -r "$_co_inc" ] || {
        log "watchtower: czar-outcome-check skipped — $_co_inc not readable"
        exit 0
    }

    # Query all czar-trigger beads (all statuses). --brief omits description/notes to
    # keep the query cheap; we only need id, status, created_at, closed_at, external_ref.
    _co_raw="$(bd -C "$SPIRA_DB" list \
        --label "$_co_label" \
        --all --json --limit 0 --brief 2>/dev/null)" || _co_raw=""

    # Parse and classify beads that need escalation.
    # Outputs: UNCLAIMED <id> <ref> or NOT_CLEARED <id> <ref>
    #
    # JSON goes to a temp file (not stdin) because `python3 - <<'PYEOF'` uses the
    # heredoc as the script source; a concurrent pipe to stdin would lose the data —
    # the heredoc takes precedence and sys.stdin.read() returns empty.
    _co_json_f="$(mktemp)"
    printf '%s\n' "${_co_raw:-[]}" > "$_co_json_f"
    _co_hits="$(python3 - "$_co_now" "$_co_outcome_mins" "$_co_unclaimed_mins" "$_co_json_f" <<'PYEOF'
import sys, json
from datetime import datetime, timezone

def ts(s):
    if not s: return None
    try: return int(datetime.strptime(s.rstrip('Z'), '%Y-%m-%dT%H:%M:%S').replace(tzinfo=timezone.utc).timestamp())
    except: return None

data = json.loads(open(sys.argv[4]).read() or '[]')
if not isinstance(data, list): data = [data]
now_s    = int(sys.argv[1])
out_secs = int(sys.argv[2]) * 60
unc_secs = int(sys.argv[3]) * 60

by_ref = {}
for b in data:
    ref = b.get('external_ref') or ''
    if not ref.startswith('incident:queue-'): continue
    b['_ct']  = ts(b.get('created_at'))
    b['_cla'] = ts(b.get('closed_at'))
    by_ref.setdefault(ref, []).append(b)

for ref, beads in by_ref.items():
    beads.sort(key=lambda b: b.get('_ct') or 0)
    newest = beads[-1]
    status = newest.get('status', '')
    ct = newest.get('_ct')

    if status in ('open', 'in_progress'):
        if ct and (now_s - ct) >= unc_secs:
            print('UNCLAIMED', newest['id'], ref)
        continue

    # Outcome check: find any closed bead followed by a newer bead after its close_at
    for i, bead in enumerate(beads):
        if bead.get('status') != 'closed': continue
        cla = bead.get('_cla')
        if not cla or (now_s - cla) < out_secs: continue
        if any(b.get('_ct') and b['_ct'] > cla for b in beads[i+1:]):
            print('NOT_CLEARED', bead['id'], ref)
            break
PYEOF
    2>/dev/null)" || _co_hits=""
    rm -f "$_co_json_f"

    while IFS=' ' read -r _co_kind _co_id _co_ref; do
        [ -n "$_co_kind" ] || continue
        case "$_co_kind" in
        UNCLAIMED)
            printf 'Czar-trigger bead %s (class: %s) has been open for more than %s minutes without being claimed or closed.\n\nThe czar'\''s summoning budget is 5 minutes. If the czar lane is not running, check: SPIRA_FAYTHS, SPIRA_LANES, and the spira-aeon-czar unit.\n\nBead: %s\nClass: %s\n' \
                "$_co_id" "$_co_ref" "$_co_unclaimed_mins" "$_co_id" "$_co_ref" | \
            SPIRA_DB="$SPIRA_DB" \
            SPIRA_INCIDENT_TYPE=task \
            SPIRA_INCIDENT_PRIORITY=1 \
            SPIRA_INCIDENT_ACTOR=watchtower \
            SPIRA_SIN_EXEMPT=1 \
            SPIRA_INCIDENT_REPO=spira \
            SPIRA_INCIDENT_REF="incident:czar-unclaimed-${_co_id}" \
            SPIRA_INCIDENT_CAUSE="czar-unclaimed" \
            bash "$_co_inc" file \
                "CZAR: trigger bead ${_co_id} unclaimed (${_co_ref})" \
                - >/dev/null || true
            log "watchtower: czar-outcome-check filed unclaimed escalation for ${_co_id}"
            ;;
        NOT_CLEARED)
            printf 'Czar closed trigger bead %s (class: %s) but the condition returned: a newer bead with the same class was filed after the closure, and the outcome window (%sm) has elapsed.\n\nThe czar'\''s action did not hold. Investigate: was the batch actually fixed, or did the same fault recur?\n\nBead: %s\nClass: %s\n' \
                "$_co_id" "$_co_ref" "$_co_outcome_mins" "$_co_id" "$_co_ref" | \
            SPIRA_DB="$SPIRA_DB" \
            SPIRA_INCIDENT_TYPE=task \
            SPIRA_INCIDENT_PRIORITY=1 \
            SPIRA_INCIDENT_ACTOR=watchtower \
            SPIRA_SIN_EXEMPT=1 \
            SPIRA_INCIDENT_REPO=spira \
            SPIRA_INCIDENT_REF="incident:czar-not-cleared-${_co_id}" \
            SPIRA_INCIDENT_CAUSE="czar-not-cleared" \
            bash "$_co_inc" file \
                "CZAR: outcome not cleared — condition returned after ${_co_id} closed (${_co_ref})" \
                - >/dev/null || true
            log "watchtower: czar-outcome-check filed not-cleared escalation for ${_co_id}"
            ;;
        esac
    done <<< "$_co_hits"

    log "watchtower: czar-outcome-check complete"
    exit 0
fi

# --pr-stall-check: PR-mode stall detector, called by sentinel.sh on every pass.
#
# Reads landstate files for REBASED pr-open:<repo> entries older than SPIRA_PR_STALL_MINS.
# For each stalled bead:
#   allow_auto_merge=false at repo level → escalate ONCE per repo via incident.sh (deduped).
#   allow_auto_merge=true but PR not armed → arm auto-merge via gh pr merge --auto --squash.
#
# Unlike --queue-checks (file reads only), this makes GitHub API calls — but only when
# stalled PRs exist, so the sentinel loop stays fast when the pipeline is moving normally.
if [ "${1:-}" = "--pr-stall-check" ]; then
    [ -f "$SPIRA_RUN/world.halted" ] && {
        log "watchtower: pr-stall-check skipped — world is halted"
        exit 0
    }

    _psc_stall_secs=$(( ${SPIRA_PR_STALL_MINS:-60} * 60 ))
    _psc_now="$(date +%s)"
    _psc_inc="${SPIRA_INCIDENT_SH:-$(dirname "$0")/incident.sh}"
    _psc_gh="${SPIRA_GH:-gh}"

    [ -r "$_psc_inc" ] || {
        log "watchtower: pr-stall-check skipped — $_psc_inc not readable"
        exit 0
    }

    if [ -d "$SPIRA_RUN/landstate" ]; then
        while IFS= read -r _psc_f; do
            [ -r "$_psc_f" ] || continue
            _psc_st=""; _psc_tip=""; _psc_at=""; _psc_reason=""
            read -r _psc_st _psc_tip _psc_at _psc_reason < "$_psc_f" 2>/dev/null || true
            [ "$_psc_st" = "REBASED" ] || continue
            case "${_psc_reason:-}" in pr-open:*) ;; *) continue ;; esac
            case "${_psc_at:-}" in ''|*[!0-9]*) continue ;; esac
            _psc_age=$(( _psc_now - _psc_at ))
            [ "$_psc_age" -ge "$_psc_stall_secs" ] 2>/dev/null || continue

            _psc_id="$(basename "$_psc_f")"
            _psc_repo="${_psc_reason#pr-open:}"
            [ -n "$_psc_repo" ] || continue
            _psc_repo_path="$(repo_root "$_psc_repo" 2>/dev/null)" || continue
            [ -e "${_psc_repo_path:-}/.git" ] || continue

            _psc_aam="$(cd "$_psc_repo_path" && \
                timeout "${GH_TIMEOUT:-120}" "$_psc_gh" repo view \
                    --json allowAutoMerge --jq .allowAutoMerge 2>/dev/null || true)"

            if [ "${_psc_aam:-}" = "false" ]; then
                printf 'PR stall: bead %s in repo %s has been waiting %s minutes.\n\nThe repository has allow_auto_merge=false. Auto-merge can never fire until it is enabled.\n\nEnable it: GitHub → repository Settings → General → Allow auto-merge.\n\nBead %s will remain stalled until this is enabled.\n' \
                    "$_psc_id" "$_psc_repo" "$(( _psc_age / 60 ))" "$_psc_id" | \
                SPIRA_DB="$SPIRA_DB" \
                SPIRA_INCIDENT_TYPE=task \
                SPIRA_INCIDENT_PRIORITY=1 \
                SPIRA_INCIDENT_ACTOR=watchtower \
                SPIRA_SIN_EXEMPT=1 \
                SPIRA_INCIDENT_REPO=spira \
                SPIRA_INCIDENT_REF="incident:pr-stall-auto-merge-off:${_psc_repo}" \
                SPIRA_INCIDENT_CAUSE=pr-stall-auto-merge-off \
                bash "$_psc_inc" file \
                    "PR STALL: ${_psc_repo} allow_auto_merge=false — enable it to unblock" \
                    - >/dev/null || true
                log "watchtower: pr-stall-check: ${_psc_repo} allow_auto_merge=false (bead ${_psc_id}, age ${_psc_age}s) — escalated"
            else
                # CONFLICTING PR: clear the landstate so landing.sh rebases and re-pushes on
                # the next pass. landing.sh's needs_refresh detects a base that has moved out
                # from under the PR branch and force-pushes a fresh rebase; removing the
                # REBASED landstate unblocks CHECK 5's guard so the branch re-enters the pass.
                _psc_mergeable="$(cd "$_psc_repo_path" && \
                    timeout "${GH_TIMEOUT:-120}" "$_psc_gh" pr view "spira/$_psc_id" \
                        --json mergeable --jq .mergeable 2>/dev/null || true)"
                if [ "${_psc_mergeable:-}" = "CONFLICTING" ]; then
                    rm -f "$_psc_f"
                    log "watchtower: pr-stall-check: ${_psc_id} in ${_psc_repo} is CONFLICTING — cleared landstate to trigger rebase"
                else
                    ( cd "$_psc_repo_path" && \
                        timeout "${GH_TIMEOUT:-120}" "$_psc_gh" pr merge --auto --squash \
                            "spira/$_psc_id" >/dev/null 2>&1 ) \
                        && log "watchtower: pr-stall-check: armed auto-merge for ${_psc_id} in ${_psc_repo}" \
                        || log "watchtower: pr-stall-check: could not arm auto-merge for ${_psc_id} in ${_psc_repo} (age ${_psc_age}s)"
                fi
            fi
        done < <(find "$SPIRA_RUN/landstate" -maxdepth 1 -type f 2>/dev/null)
    fi

    log "watchtower: pr-stall-check complete"
    exit 0
fi

SNAP_AGE_MAX="${SPIRA_SNAP_STALE_S:-60}"
now="$(date +%s)"

# ---------------------------------------------------------------------------------------
# HALTED? A deliberately stopped world must not manufacture incidents. Every pipeline
# metric grows monotonically while nothing is wrong — time since last landing, queue depth
# — so a sweep against a halted world describes a system that is broken when it is not.
# cockpit/health.sh reads the same stamp directly for the same reason: a stale or absent
# snapshot must not mask a deliberate halt.
# ---------------------------------------------------------------------------------------
HALT_STAMP="$SPIRA_RUN/world.halted"
halt_since=""; halt_why=""
if [ -f "$HALT_STAMP" ]; then
    halt_since="$(head -1 "$HALT_STAMP" 2>/dev/null)"
    halt_why="$(sed -n '2s/^why: //p' "$HALT_STAMP" 2>/dev/null)"
fi

# ---------------------------------------------------------------------------------------
# DRAINING? Drain is lighter than halt — the loop, landing and reaping continue; only new
# summons are gated. A drain left armed longer than intended has the same shape as
# law-arm-before-you-retire: a stopped channel and a quiet one are indistinguishable from
# outside, so the vital sign belongs here alongside the halt signal.
#
# READ THE STAMP DIRECTLY, NOT world.sh STATUS. world.sh status reads systemd unit names,
# which are broken (sp-4biz, P0). drain and resume work correctly because they gate on the
# stamp and never name a unit, so we do the same.
#
# MINUTES FROM MTIME, same as cockpit/health.sh: `stat -c %Y` returns epoch seconds the OS
# recorded when the file was written, which needs no date parsing. A failed stat renders `?`
# — never "not draining" (law-absence-needs-a-positive-control).
# ---------------------------------------------------------------------------------------
DRAIN_STAMP="$SPIRA_RUN/world.draining"
# drain_since empty → no stamp → not draining; drain_mins "?" → stamp exists but unreadable.
# Not-draining renders 0, not "?" — the ? convention is for a probe that FAILED, not for the
# absence of the condition being probed (law-absence-needs-a-positive-control).
drain_since=""; drain_mins=0
if [ -f "$DRAIN_STAMP" ]; then
    drain_since="$(head -1 "$DRAIN_STAMP" 2>/dev/null)"
    _dmtime="$(stat -c %Y "$DRAIN_STAMP" 2>/dev/null)"
    if [ -n "$_dmtime" ] && [ "$_dmtime" -gt 0 ] 2>/dev/null; then
        drain_mins=$(( (now - _dmtime) / 60 ))
    else
        drain_mins="?"
    fi
fi

# How long before a drain triggers its own incident. The normal sweep already carries the
# drain state as a vital sign; this threshold is the point at which the sweep alone is not
# enough and an escalation bead is worth the noise.
DRAIN_WARN_MINS="${SPIRA_DRAIN_WARN_MINS:-15}"

# How old the oldest unsent branch must be (in hours) before the Sending escalation fires.
# An unsent branch belonging to a live in_progress bead is work in flight; the escalation is
# for branches that have been waiting far longer than any single bead should take.
UNSENT_WARN_H="${SPIRA_UNSENT_WARN_H:-24}"
CLOSED_STRANDED_WARN_H="${SPIRA_CLOSED_STRANDED_WARN_H:-48}"

# ---------------------------------------------------------------------------------------
# THE COLLECTOR'S SNAPSHOT, and whether it can be believed at all. Every other number below
# is read out of cockpit.env, so its freshness is the first fact — a stale file makes the
# whole sweep a report about the past, and reporting the past as the present during an
# outage is worse than reporting nothing.
# ---------------------------------------------------------------------------------------
ENVF="$SPIRA_RUN/cockpit/cockpit.env"
[ -r "$ENVF" ] || ENVF="$SPIRA_RUN/cockpit.env"
snap_age="?"
if [ -r "$ENVF" ]; then
    # shellcheck disable=SC1090
    eval "$(sed -n 's/^\(SP_[A-Z_0-9]*\)=\(.*\)$/\1=\2/p' "$ENVF" 2>/dev/null)" 2>/dev/null || true
    [ -n "${SP_AT:-}" ] && snap_age=$(( now - SP_AT ))
fi
snap_age_disp="${snap_age}s"
if [ "$snap_age" != "?" ] && [ "$snap_age" -ge "$SNAP_AGE_MAX" ] 2>/dev/null; then
    snap_age_disp="FAULT (${snap_age}s, stale above ${SNAP_AGE_MAX}s)"
fi
g() { local v="${!1:-}"; [ -n "$v" ] && printf '%s' "$v" || printf '?'; }
# In-flight count: total unsent minus the closed-bead stranded ones. Both must be numeric;
# a `?` on either renders the in-flight figure as `?` rather than a false arithmetic result.
_unsent_inflight="?"
if [ "${SP_UNSENT:-?}" != "?" ] && [ "${SP_CLOSED_STRANDED:-?}" != "?" ] 2>/dev/null; then
    _unsent_inflight=$(( SP_UNSENT - SP_CLOSED_STRANDED ))
fi

# ---------------------------------------------------------------------------------------
# HOW LONG SINCE ANYTHING LANDED — the one number that says whether the pipeline works, and
# the one nothing recorded until landing.sh began writing a landstate file per bead. Read
# from those records rather than from the log, because a log line is prose and this has to
# be arithmetic.
# ---------------------------------------------------------------------------------------
# THE RECORD HAS NO TRAILING NEWLINE, and `read` reports that as failure. land_mark writes
# with `printf '%s %s %s %s'` deliberately — the in-tree reader strips newlines anyway — so
# every landstate file ends mid-line, and `read` returns 1 at EOF-without-delimiter EVEN
# THOUGH IT HAS ALREADY POPULATED EVERY VARIABLE. A `|| continue` on that status therefore
# discarded a perfectly good record, and this field rendered `?  (last: none recorded)` on
# every sweep ever filed, including passes where six beads had landed in the previous twelve
# minutes. Three Ops sessions were woken by it, each one re-deriving the same directory by
# hand to prove the pipeline was moving.
#
# So the reader tolerates the failed status and lets the guards below it judge the content:
# a record is believed only if its state is LANDED and its timestamp is numeric, which a
# truncated or empty file cannot satisfy. Do not "fix" this by adding a newline to the
# writer — two readers already depend on the current format and the writer is not wrong.
#
# THE FOUR VARIABLES ARE RESET BEFORE EACH READ, and that is load-bearing rather than tidy:
# `read` leaves the previous iteration's values in place when it fails early, so an
# unreadable file would otherwise be judged on the LAST file's state and this loop would
# attribute one bead's landing to another.
last_land="?"; last_land_id=""
if [ -d "$SPIRA_RUN/landstate" ]; then
    while IFS= read -r f; do
        [ -r "$f" ] || continue
        st=""; _tip=""; at=""; _why=""
        read -r st _tip at _why < "$f" 2>/dev/null || true
        [ "$st" = LANDED ] || continue
        case "$at" in ''|*[!0-9]*) continue ;; esac
        if [ "$last_land" = "?" ] || [ "$at" -gt "$last_land" ]; then
            last_land="$at"; last_land_id="$(basename "$f")"
        fi
    done < <(find "$SPIRA_RUN/landstate" -maxdepth 1 -type f 2>/dev/null)
fi
since_land="?"
[ "$last_land" != "?" ] && since_land=$(( (now - last_land) / 60 ))

# ---------------------------------------------------------------------------------------
# WHAT IS STUCK, AND FOR HOW LONG. A queue depth on its own says nothing — a deep queue that
# is moving is a busy system. The age of its oldest member is what tells them apart.
# ---------------------------------------------------------------------------------------
# THE WINDOW IS A DURATION, NEVER A ROW COUNT. "The worst wait in the last 50 rows" reads as
# recent and is not: gate.log holds one row per gate run, so on a quiet day fifty rows are a
# week and "recent" silently means "ever". That is not hypothetical either — this field spent
# a day reporting 1584s from a wait produced by a locking topology the gate rebuild had
# already deleted, while every row written since read `waited=0s`. A decommissioned
# mechanism's worst case was being presented to Ops as a live signal, and three sweeps
# re-investigated it.
#
# THE CUTOFF IS COMPARED AS A STRING, which is exactly as sound as arithmetic here and needs
# no date parsing in awk: the meter writes `date -u +%Y-%m-%dT%H:%M:%SZ`, and ISO-8601 UTC
# timestamps of fixed width sort lexicographically in chronological order. A row whose first
# field is not such a timestamp — a truncated write, a line from some older format — falls
# outside every window and is ignored rather than counted as now.
#
# NO ROW INSIDE THE WINDOW RENDERS `?`, NOT 0. "No gate has waited recently" and "no gate has
# RUN recently" are opposite facts and a zero states the reassuring one (law-absence-needs-a-
# positive-control); the second is what a stalled pipeline looks like from here.
GATE_WINDOW="${SPIRA_WATCH_GATE_WINDOW:-21600}"
# THROUGH THE SAME KEY THE METER WRITES. This read `$SPIRA_RUN/gate.log` directly, so an
# operator who moved the log left this field reading `?` forever while the gate went on
# writing somewhere else — a probe pointed at the wrong place, which is the failure the whole
# `?` convention exists to make visible rather than one it is allowed to have.
WT_GATE_LOG="${SPIRA_GATE_LOG:-$SPIRA_RUN/gate.log}"
oldest_wait="?"; oldest_br=""
if [ -r "$WT_GATE_LOG" ]; then
    gate_since="$(date -u -d "@$(( now - GATE_WINDOW ))" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)"
    if [ -n "$gate_since" ]; then
        read -r oldest_wait oldest_br < <(awk -v since="$gate_since" '
            $1 >= since && $1 ~ /^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:]+Z$/ &&
            match($0, /waited=[0-9]+s/) {
                w = substr($0, RSTART+7, RLENGTH-8) + 0
                if (!n++ || w > m) { m = w; b = $3 }
            } END { if (n) print m, b; else print "?", "" }' "$WT_GATE_LOG" 2>/dev/null)
    fi
fi
# What the field PRINTS, decided here rather than in the heredoc, so that a `?` is not
# rendered as `?s` — a unit on an unreadable field invites reading it as a measurement.
gate_wait_disp="?"
[ "$oldest_wait" = "?" ] || gate_wait_disp="${oldest_wait}s"
# The window is NAMED in the field, because "recent" is the word that let this go wrong: a
# reader who can see the bound can tell a quiet six hours from a broken probe.
if [ "$GATE_WINDOW" -ge 3600 ] 2>/dev/null; then gate_win_label="last $(( GATE_WINDOW / 3600 ))h"
else gate_win_label="last $(( GATE_WINDOW / 60 ))m"; fi

# ---------------------------------------------------------------------------------------
# IS THE GATE WORTH WHAT IT COSTS? The wait above says what the gate costs the queue; these
# say whether it is buying anything. A gate whose reds are mostly its own fault has negative
# value and can be deleted in a sentence on this evidence, instead of after twelve hours of
# fallout — which is how the last one went (law-gate-earns-its-place).
#
# READ BY OPS, NOT ONLY BY A HUMAN AT A PANE. Ops is the actor that reads this sweep and cuts
# beads from it; a yield that could only be seen by somebody who went looking would be the
# same failure one layer up, since going to look is exactly what nobody did.
#
# yield.sh renders `?` for anything it could not read and this passes that through unchanged.
# SPIRA_RUN is passed explicitly because conf.sh does not export it, and a child re-deriving
# it would read a different directory and report a confident zero.
YIELD_REDS="?"; YIELD_DEFECT="?"; YIELD_FAULT="?"; YIELD_UNKNOWN="?"; YIELD_RECORDER="?"
YIELD_DEFECT_INFERRED="?"; YIELD_TOP_FAULT="?"
YIELD_SOLO_N="?"; YIELD_SOLO_MED="?"; YIELD_SOLO_MAX="?"
YIELD_CONC_N="?"; YIELD_CONC_MED="?"; YIELD_CONC_MAX="?"
YIELD_SH="$(dirname "$0")/yield.sh"
YIELD_WINDOW_S="${SPIRA_YIELD_WINDOW:-86400}"
if [ -r "$YIELD_SH" ]; then
    # shellcheck disable=SC1090
    eval "$(SPIRA_RUN="$SPIRA_RUN" SPIRA_YIELD_WINDOW="$YIELD_WINDOW_S" \
            bash "$YIELD_SH" report 2>/dev/null \
            | sed -n 's/^\(YIELD_[A-Z_]*\)=\(.*\)$/\1="\2"/p')" 2>/dev/null || true
fi
if [ "$YIELD_WINDOW_S" -ge 86400 ] 2>/dev/null; then yield_win_label="last $(( YIELD_WINDOW_S / 86400 ))d"
elif [ "$YIELD_WINDOW_S" -ge 3600 ] 2>/dev/null; then yield_win_label="last $(( YIELD_WINDOW_S / 3600 ))h"
else yield_win_label="last $(( YIELD_WINDOW_S / 60 ))m"; fi
# A UNIT ON AN UNREADABLE FIELD INVITES READING IT AS A MEASUREMENT — `?s` looks like a
# duration somebody forgot to fill in, and this whole file exists because a reassuring
# reading displaced a look.
secs() { [ "${1:-?}" = "?" ] && printf '?' || printf '%ss' "$1"; }
# A `?` WITH NO EXPLANATION SENDS OPS TO THE CODE. The yield's own positive control is the
# gate meter, which writes a row on every red whether or not anything is measuring; when the
# two disagree the count is withheld, and this is the sentence that says which of them was
# silent so the sweep names the fault rather than the symptom.
yield_note_txt=""
case "$YIELD_RECORDER" in
    silent) yield_note_txt="   <- the gate meter saw ${YIELD_LOG_REDS:-?} red(s) and none reached the record: THE RECORDER IS NOT RUNNING" ;;
    absent) yield_note_txt="   <- nothing recorded here yet, and the meter has logged no reds either" ;;
    '?')    yield_note_txt="   <- no positive control: the gate meter could not be read" ;;
esac

# Branches whose gate could not reach a verdict, and how many times in a row. Written by the
# landing pass; three of one reason on one branch is what it escalates on.
nv_worst=0; nv_worst_key=""
if [ -d "$SPIRA_RUN/noverdict" ]; then
    while IFS= read -r f; do
        case "$f" in *.asked) continue ;; esac
        n="$(cat "$f" 2>/dev/null)"; case "$n" in ''|*[!0-9]*) continue ;; esac
        [ "$n" -gt "$nv_worst" ] && { nv_worst="$n"; nv_worst_key="$(basename "$f")"; }
    done < <(find "$SPIRA_RUN/noverdict" -maxdepth 1 -type f 2>/dev/null)
fi

# AEONS ARE COUNTED HERE, FROM /proc, NOT READ OUT OF THE SNAPSHOT. The collector's file is
# up to a minute old, and the first live sweep reported "0 aeons" while two were running —
# because the snapshot predated the summon. Every other number here tolerates being a minute
# stale; this one does not, because "ready work and no workers" is the single shape that
# most looks like a stalled loop, and reporting it wrongly sends Ops to diagnose a stall that
# is not happening. It costs one pass over /proc.
#
# THROUGH aeon_count, THE HARNESS'S OWN PRIMITIVE, not a second implementation. The first
# version of this scanned /proc for a command line containing "aeon.sh" and counted 7 where
# there were 3 — because the scan matched the shell pipelines that were themselves grepping
# for the string, this program's own diagnostics included. That is the `pgrep -f` failure
# exactly, arriving in a hand-rolled shape, three lines under a comment warning about it.
#
# aeon_count reads the pidfiles, which are the authoritative record of a claim, and confirms
# each against /proc on argv rather than on a substring. It is also what the sentinel's pool
# arithmetic uses, so the number reported here and the number the loop acts on cannot drift.
aeons_live=0
for _f in $(spira_fayths 2>/dev/null); do
    aeons_live=$(( aeons_live + $(aeon_count "$_f" 2>/dev/null || echo 0) ))
done

# ---------------------------------------------------------------------------------------
# THE MENU. The sweep names scans for the Ops session to RUN; it does not run them here.
#
# That division is the whole point and it is not decoration. This program's contract is to be
# deterministic and cheap, because the one thing a detector may not be is another thing that
# is down during an outage — a several-minute test run inside it would make it exactly that,
# and would push a ten-minute cadence past the interval that produces it. So the menu is a
# NAME plus the cheap facts that say whether the scan is worth this pass, and the session
# spends its own eight minutes on it.
#
# `suites.sh status` is a glob and a read per suite: no database, no network, nothing that
# can hang. A pass that cannot produce it prints why rather than an empty section, because a
# menu with nothing on it and a menu that could not be built read identically otherwise.
# SPIRA_SUITES_SH overrides the path so test suites can inject a mock without paying the
# host-check.sh walk on every watchtower.sh invocation. Same seam as SPIRA_INCIDENT_SH.
SUITES="${SPIRA_SUITES_SH:-$(dirname "$0")/suites.sh}"
suites_block="  (unavailable — $SUITES is missing, so nothing knows which suites run nowhere)"
if [ -r "$SUITES" ]; then
    suites_block="$(bash "$SUITES" status 2>/dev/null)"
    [ -n "$suites_block" ] || suites_block="  (unreadable — suites.sh status produced nothing)"
fi

# THE STRAND LEDGER IS TWO LINES, NOT ONE. strands.json holds every disposition strand.sh
# classifies, and only `ghost` is the labelled failure this line names — a claimed bead whose
# holder is gone. This rendered the ledger's SIZE under that name, so a childless epic read
# as a dead worker and a sweep spent four commands hunting for a holder that never existed.
# The collector does the classifying (cockpit.sh strand_keys); this only renders it.
#
# A SNAPSHOT WRITTEN BY A COLLECTOR PREDATING THAT SPLIT RENDERS `?`, WHICH IS CORRECT: `g`
# reports an absent key as unread, and during a rollout the two halves are briefly skewed.
# `?` says this pass could not read it. A 0 would say there are none, which nobody checked.

# BRANCH INTEGRITY — two cheap reads per registered repository: is the base branch's tip
# a non-merge aeon commit, and is the shared checkout ahead of its remote? Both were
# invisible on the day this defect was filed; both are one git command each.
#
# COMPUTED HERE AND NOT INLINE IN THE HEREDOC. Command substitution inside a here-doc
# expands at the wrong time on some shells, and a multiline output inside $( ) would close
# the here-doc prematurely. Pre-computed variables avoid both.
#
# MISSING GUARD RENDERS A NOTE, NEVER SILENCE. A guard whose script is absent is not the
# same as a guard that ran and found nothing (law-absence-needs-a-positive-control).
GUARD_SH="$(dirname "$0")/branch-guard.sh"
guard_block="  (unavailable — branch-guard.sh is missing or unreadable)"
if [ -r "$GUARD_SH" ]; then
    guard_out="$(bash "$GUARD_SH" check 2>&1)"; guard_rc=$?
    case "$guard_rc" in
        0) guard_block="  $guard_out" ;;
        3) guard_block="  (no registered repositories — nothing to audit)" ;;
        *) guard_block="$(printf '%s\n' "$guard_out" | sed 's/^/  /')" ;;
    esac
fi

# THE CZAR TRIGGER TABLE — per-class display of the four queue-check trigger classes.
# Read from cockpit.env (written by czar_triggers_keys via collect.sh). A missing key
# renders ? (law-absence-needs-a-positive-control): the collector not having run is not
# the same as no czar events having occurred.
_cz_row() {  # _cz_row <label> <tag>  -> one table line
    local _label="$1" _tag="$2"
    printf '  %-22s %-22s %-12s %s\n' \
        "$_label" \
        "$(g "SP_CZAR_${_tag}_FIRED")" \
        "$(g "SP_CZAR_${_tag}_BY")" \
        "$(g "SP_CZAR_${_tag}_OUTCOME")"
}
czar_block="$(printf '  %-22s %-22s %-12s %s\n' class "last fired" "handled by" outcome)
$(_cz_row deadlock          DEADLOCK)
$(_cz_row attribution-failed ATTRIB)
$(_cz_row sort-failed        SORT)
$(_cz_row loop-stalled       STALL)"

# Pre-computed so the heredoc below can reference it as a plain variable. A trailing
# newline is intentional: the heredoc adds one more, giving a blank line between the halt
# banner and the body text.
halt_section=""
if [ -n "$halt_since" ]; then
    halt_section="!! HALTED since ${halt_since}"
    [ -n "$halt_why" ] && halt_section="${halt_section}
   why: ${halt_why}"
    halt_section="${halt_section}
   No incidents are filed while the halt is in force.
"
fi

# ---------------------------------------------------------------------------------------
# LAPSED AEONS — aeons the liveness lease killed since the previous sweep.
#
# sp-a8zy writes a record under $SPIRA_RUN/lapsed/ on each lapse, named
# <bead-id>-<YYYYMMDDTHHmmSSZ>. This section reads those whose timestamp is newer than the
# marker in $SPIRA_RUN/lapsed.swept (the previous sweep's cutoff), so each lapse appears in
# exactly one sweep.
#
# ? WHEN THE DIRECTORY CANNOT BE READ — not 0, not silence. A section that renders empty
# on a failed read displaces the suspicion that would prompt a look, which is the exact
# failure mode this whole program is a response to (law-absence-needs-a-positive-control).
#
# THE MARKER IS CAPTURED NOW, before reading records, and written after the prompt file is
# written atomically. That ordering ensures a lapse that arrives while this sweep is running
# falls in the next pass rather than being lost — and that the marker never advances if the
# write fails (so the record stays visible on retry).
#
# THE CUTOFF IS A LEXICOGRAPHIC COMPARISON. Timestamps are YYYYMMDDTHHmmSSZ — ISO-8601 UTC
# compact format — which sorts correctly as strings. No date parsing is needed: a later
# timestamp always sorts after an earlier one in this format.
#
# CONFIGURED PATHS, NOT LITERALS. `sop.sh write` scans for absolute paths; a literal would
# fail the inventory gate on every landing and on every colleague's clone.
# ---------------------------------------------------------------------------------------
LAPSED_DIR="${SPIRA_LAPSED_DIR:-$SPIRA_RUN/lapsed}"
LAPSED_MARKER="${SPIRA_LAPSED_MARKER:-$SPIRA_RUN/lapsed.swept}"
_lapsed_prev="$(cat "$LAPSED_MARKER" 2>/dev/null || true)"
_lapsed_now="$(date -u +%Y%m%dT%H%M%SZ)"

lapsed_count="?"
lapsed_section=""
if [ ! -e "$LAPSED_DIR" ]; then
    # No lapses ever recorded — the directory is created by aeon.sh on the first lapse.
    lapsed_count=0
    lapsed_section="  none since last sweep"
elif [ ! -d "$LAPSED_DIR" ]; then
    # Path exists but is not a directory — something replaced it; treat as unreadable.
    lapsed_count="?"
    lapsed_section="  ? (expected a directory at SPIRA_LAPSED_DIR — path exists but is not a directory)"
else
    # Directory exists; read records newer than the previous marker.
    _lapsed_n=0
    _lapsed_body=""
    while IFS= read -r _lf; do
        [ -r "$_lf" ] || continue
        _lfname="$(basename "$_lf")"
        # Timestamp suffix is the last 16 chars of the basename (YYYYMMDDTHHmmSSZ).
        _lfts="$(printf '%s' "$_lfname" | grep -oE '[0-9]{8}T[0-9]{6}Z$' 2>/dev/null || true)"
        [ -n "$_lfts" ] || continue
        # Skip records at or before the previous marker (already shown in a prior sweep).
        # String comparison on ISO-8601 timestamps is chronological — no date math needed.
        [ -z "$_lapsed_prev" ] || [[ "$_lfts" > "$_lapsed_prev" ]] || continue
        _lapsed_n=$(( _lapsed_n + 1 ))
        _lapsed_body="${_lapsed_body}
  [${_lfname}]
$(sed 's/^/    /' "$_lf" 2>/dev/null)
"
    done < <(find "$LAPSED_DIR" -maxdepth 1 -type f 2>/dev/null | sort)
    lapsed_count="$_lapsed_n"
    if [ "$_lapsed_n" -eq 0 ]; then
        lapsed_section="  none since last sweep"
    else
        [ "$_lapsed_n" -eq 1 ] && _lapse_word="lapse" || _lapse_word="lapses"
        lapsed_section="  ${_lapsed_n} ${_lapse_word} since last sweep:
${_lapsed_body}"
    fi
fi

# Drain section: present only when the stamp exists. Unlike the halt section, a draining
# world still files its sweep — the loop and landing continue. The section is a warning
# banner, not a suppression notice.
drain_section=""
if [ -n "$drain_since" ]; then
    drain_section="!! DRAINING since ${drain_since} (${drain_mins}m)
   Summons gated; loop, landing and reaping continue. Lift with: world.sh resume
"
fi

# THROTTLE STATE — read from the stamp file written by --throttle-check (called each sentinel
# pass). A throttled queue and an empty queue are indistinguishable from outside without this.
# READ DIRECTLY, NOT FROM cockpit.env: the stamp is written on the sentinel cadence (~2m), so
# it is at most one pass stale, while the snapshot may be up to the collector interval stale.
THROTTLE_STAMP="${SPIRA_THROTTLE_STAMP:-$SPIRA_RUN/queue-throttled}"
throttle_since=""; throttle_depth=""; throttle_drain=""
if [ -f "$THROTTLE_STAMP" ]; then
    _ts_line="$(head -1 "$THROTTLE_STAMP" 2>/dev/null)"
    throttle_since="$(printf '%s' "$_ts_line" | grep -oE 'since=[^ ]+' | cut -d= -f2)"
    throttle_depth="$(printf '%s' "$_ts_line" | grep -oE 'depth=[0-9]+' | cut -d= -f2)"
    throttle_drain="$(printf '%s' "$_ts_line" | grep -oE 'since_land=[0-9]+m' | cut -d= -f2)"
fi
throttle_section=""
if [ -n "$throttle_since" ]; then
    throttle_section="!! THROTTLED since ${throttle_since}
   Builder admission held (depth=${throttle_depth:-?}, drain was ${throttle_drain:-?} ago). Lifts automatically.
   Override: SPIRA_QUEUE_THROTTLE_OVERRIDE=off in spira.conf
"
elif [ "${SPIRA_QUEUE_THROTTLE_OVERRIDE:-}" = "off" ]; then
    throttle_section="   throttle: OVERRIDE OFF (SPIRA_QUEUE_THROTTLE_OVERRIDE=off)
"
fi

# /tmp usage — EDQUOT fires before df says full (per-user quota on tmpfs).
_tmp_pct="$(df /tmp 2>/dev/null | awk 'NR==2{print $5}' || true)"
[ -n "$_tmp_pct" ] || _tmp_pct="?"

# ROOT DISK AND MEMORY. A full root disk kills every process on the box, not only Spira's —
# this is a fact worth Ops seeing even though nothing here withholds a summon over it (that
# is the admission throttle's job, CHECK 7 in sentinel.sh).
DISK_WARN_PCT="${SPIRA_DISK_WARN_PCT:-90}"
MEM_WARN_MB="${SPIRA_MEM_WARN_MB:-1500}"
# SPIRA_MEMINFO_PATH is a seam for tests, the same idea as SPIRA_INCIDENT_SH and
# SPIRA_SUITES_SH above: /proc/meminfo cannot be stubbed by PATH the way df can.
MEMINFO="${SPIRA_MEMINFO_PATH:-/proc/meminfo}"
_disk_root_pct="$(df --output=pcent / 2>/dev/null | tail -1 | tr -dc '0-9')"
[ -n "$_disk_root_pct" ] || _disk_root_pct="?"
_mem_avail_mb="$(awk '/MemAvailable/{printf "%d", $2/1024}' "$MEMINFO" 2>/dev/null)"
[ -n "$_mem_avail_mb" ] || _mem_avail_mb="?"

_disk_breach=0
if [ "$_disk_root_pct" != "?" ] && [ "$_disk_root_pct" -ge "$DISK_WARN_PCT" ] 2>/dev/null; then
    _disk_breach=1
fi
_disk_disp="${_disk_root_pct}%"
[ "$_disk_breach" = 1 ] && _disk_disp="FAULT (${_disk_root_pct}%, warn at ${DISK_WARN_PCT}%)"

_mem_breach=0
if [ "$_mem_avail_mb" != "?" ] && [ "$_mem_avail_mb" -lt "$MEM_WARN_MB" ] 2>/dev/null; then
    _mem_breach=1
fi
_mem_disp="${_mem_avail_mb}MB"
[ "$_mem_breach" = 1 ] && _mem_disp="FAULT (${_mem_avail_mb}MB, warn below ${MEM_WARN_MB}MB)"

snapshot() {
cat <<EOF
## Spira pipeline, $(date -u +%Y-%m-%dT%H:%M:%SZ)
${halt_section}${drain_section}${throttle_section}
N workers pull from a DAG into a merge queue. These are that queue's vital signs. A field
reading \`?\` is one this pass COULD NOT READ — never treat it as a zero.

### The far end — is anything coming out?

  minutes since the last landing      ${since_land}      (last: ${last_land_id:-none recorded})
  branches finished but not landed    $(g SP_UNLANDED_N)
  branches done and waiting           $(g SP_BRANCH_DONE)
  $(printf '%-36s' "longest gate wait, $gate_win_label")${gate_wait_disp}   ${oldest_br:-}
  worst no-verdict streak             ${nv_worst}       ${nv_worst_key:-none}

### The Sending — are finished branches leaving?

  SP_UNSENT is the total; the two rows below break it into work in flight (open bead,
  may still land) vs stranded (closed bead, sending.sh has not reaped it yet).
  A no-bead branch splits into two kinds: one whose commits are already on the base
  (SP_UNADOPTED — safe to delete) and one whose commits are absent (SP_ORPHAN_WORK —
  unlanded work; deletion would destroy commits). Only SP_UNADOPTED triggers the reap
  escalation.

  unsent branches (total)             $(g SP_UNSENT)
    in-flight (open bead)             ${_unsent_inflight}
    stranded (closed bead)            $(g SP_CLOSED_STRANDED)
  oldest in-flight (hours)            $(g SP_UNSENT_OLDEST_H)
  oldest stranded (hours)             $(g SP_CLOSED_STRANDED_OLDEST_H)
  BATCHED with no open batch          $(g SP_BATCHED_STRANDED)
  BATCHED longer than one batch pass  $(g SP_BATCHED_TOO_LONG)
  strays (no bead, commits on base)   $(g SP_UNADOPTED)
  orphan work (no bead, has commits)  $(g SP_ORPHAN_WORK)
  fiends (FAILED deletes, came back)  $(g SP_SENT_FAILED)

### The gate — is it buying anything?

  UNKNOWN is never folded into either column. A run of them means this measurement has
  itself stopped working, which is the one thing a yield figure must not hide.

  $(printf '%-36s' "gate reds, $yield_win_label")${YIELD_REDS}${yield_note_txt}
  $(printf '%-36s' "  the branch really was wrong")${YIELD_DEFECT}      (${YIELD_DEFECT_INFERRED} inferred from a later pass, not stated)
  $(printf '%-36s' "  the gate's own fault")${YIELD_FAULT}      worst: ${YIELD_TOP_FAULT}
  $(printf '%-36s' "  never classified")${YIELD_UNKNOWN}
  $(printf '%-36s' "gate cost, solo")$(secs "$YIELD_SOLO_MED") median, $(secs "$YIELD_SOLO_MAX") worst (n=${YIELD_SOLO_N})
  $(printf '%-36s' "gate cost, another gate overlapping")$(secs "$YIELD_CONC_MED") median, $(secs "$YIELD_CONC_MAX") worst (n=${YIELD_CONC_N})

### The workers

  /tmp used (? = cannot read)         ${_tmp_pct}
  / used (? = cannot read)            ${_disk_disp}
  memory available (? = cannot read)  ${_mem_disp}
  throttle                            ${throttle_since:-clear}      (stamp: queue-throttled; depth at engage: ${throttle_depth:-—})
  draining since (? = cannot read)    ${drain_mins}      minutes   (stamp: world.draining)
  aeons alive                         ${aeons_live}      (counted now, not from the snapshot)
  beads in progress                   $(g SP_INPROG)
  ready to claim                      $(g SP_READY)
  poisoned                            $(g SP_POISON)
  stranded (claimed, nobody home)     $(g SP_STRAND_GHOST)
  strand ledger, other classes        $(g SP_STRAND_OTHER)
  account capacity paused             $(g SP_CAPACITY_PAUSED)

### The czar — trigger outcome

  A \`?\` means this probe could not read the store. A \`no\` means the condition
  returned after the czar closed its bead — file an investigation bead.

${czar_block}

### Lapsed aeons — killed by the liveness lease since the previous sweep

  An aeon whose trace was silent for the full lease is killed and its work preserved.
  Classify each into one of four outcomes (sop-lapsed-aeon-postmortem) — a lapse is
  never closed as "noted". ? = directory could not be read, not zero lapses.

${lapsed_section}

### The graph

  open $(g SP_OPEN) · closed $(g SP_CLOSED) · landed $(g SP_LANDED) · $SPIRA_ASK_LABEL $(g SP_NEEDSOP)
  repo: unmapped $(g SP_REPO_UNMAPPED) · absent $(g SP_REPO_ABSENT)
  parked on CI $(g SP_AWAITING_N), oldest $(g SP_AWAITING_AGE), stuck $(g SP_AWAITING_STUCK)
  duplicate incident refs             $(g SP_DUP_REFS)      (surplus beads: $(g SP_DUP_BEADS))

### The menu — run these scans, then look for what they do not cover

A sweep is not only a set of numbers to read. These are the scans that are worth sampling
before anything else, because each answers a question the numbers above cannot.

  (the timed suite run is SUSPENDED — do not run it)

    suites.sh run covers every spira/test-*.sh the landing gate does NOT run.

    Per Ryan, 2026-09-12: the timed suite run is suspended while the suite-red backlog is
    cleaned up. spira-suites-prod.timer is stopped and disabled, and this menu entry is
    withdrawn because it was the OTHER trigger — the Ops session running it on every sweep,
    which is what kept filing beads after the timer was slowed. Do not run suites.sh run.
    Re-enable via sp-jxia when the backlog clears.

$suites_block

### The shared checkout — are base branches clean?

  An aeon commit on a base branch bypasses the gate and every landing instrument. A checkout
  ahead of its remote means subsequent worktrees base on a ref nobody else has seen.

${guard_block}

### Can this snapshot be believed?

  collector snapshot age              ${snap_age_disp}
  sentinel timer                      $(g SP_SENTINEL_TIMER)   last pass $(g SP_SENTINEL_AGE)s ago

## Your task

Read the vital signs above and decide whether anything needs attention.

If the pipeline is nominal: print one line saying so and exit. No bead is needed. Silence is
not acceptable — a session that found nothing must still say so, because silence and a greeting
are indistinguishable in the log.

If something needs attention: file one bead per finding with its evidence. Name what is wrong,
the number that was anomalous, and what it means. A '?' field means this sweep could not read
it — investigate why before filing a bead on the absence alone.

Run the scans named in the menu above if you have wall time remaining.
EOF
}

[ "${1:-}" = "--show" ] && { snapshot; exit 0; }

# A HALTED WORLD MUST NOT FILE. The halt stamp is the authoritative record; checking it
# here rather than relying on the snapshot's staleness means a slow or dead collector
# cannot make a deliberate halt look like an anomaly worth escalating.
if [ -n "$halt_since" ]; then
    log "watchtower: halted since ${halt_since} (${halt_why:-why unstated}) — sweep skipped"
    exit 0
fi

# ---------------------------------------------------------------------------------------
# SKIP WHEN NOMINAL. Every signal computed above is a positive measurement; '?' means a
# probe failed. Nominal requires all four: no lapses since the last pass (lapsed_count=0),
# a fresh readable snapshot (snap_age is numeric and below its limit), no stuck no-verdict
# branches (nv_worst=0), and no drain in force. A single '?' or a nonzero count anywhere
# in that set means not-nominal — the model session is reserved for conditions the cheap
# checks can see but cannot resolve.
#
# When nominal: write a SWEEP:NOMINAL marker so the ops service can skip the model call
# rather than paying a full context to confirm a green report is green. The marker line
# carries the counts that satisfied the check so the log is self-explaining.
#
# THE MARKER ADVANCES HERE regardless of whether a full sweep runs. If it did not, the next
# pass would see the same lapse records again and report lapsed_count > 0, making the
# pipeline look unhealthy on the very first sweep after a nominal one.
# ---------------------------------------------------------------------------------------
nominal=0
if [ "$lapsed_count" = "0" ] && \
   [ "$snap_age" != "?" ] && [ "$snap_age" -lt "$SNAP_AGE_MAX" ] 2>/dev/null && \
   [ "$nv_worst" = "0" ] && \
   [ -z "$drain_since" ] && \
   [ -z "$throttle_since" ] && \
   [ "$_disk_breach" = 0 ] && \
   [ "$_mem_breach" = 0 ]; then
    nominal=1
fi

# SPIRA_INCIDENT_SH overrides the path so test suites can inject a mock without reaching
# a real database. Same seam sentinel.sh carries for systemctl.
INC="${SPIRA_INCIDENT_SH:-$(dirname "$0")/incident.sh}"
PROMPT_FILE="${SPIRA_WATCH_PROMPT_FILE:-$SPIRA_RUN/ops-sweep-prompt.txt}"

if [ "$nominal" = "1" ]; then
    printf 'SWEEP:NOMINAL lapsed=%s snap_age=%ss nv_worst=%s\n' \
        "$lapsed_count" "$snap_age" "$nv_worst" > "${PROMPT_FILE}.tmp" 2>/dev/null && \
        mv -f "${PROMPT_FILE}.tmp" "$PROMPT_FILE" || true
    printf '%s\n' "$_lapsed_now" > "$LAPSED_MARKER" 2>/dev/null || true
    log "watchtower: nominal — no sweep needed (lapsed=${lapsed_count} snap_age=${snap_age}s nv_worst=${nv_worst})"
    exit 0
fi

# ---------------------------------------------------------------------------------------
# WRITE THE SNAPSHOT AS THE SWEEP PROMPT. The ops unit hands this file to `aeon.sh --sweep`
# so Ops starts with the current pipeline picture rather than gathering it again minutes
# later. Written atomically (tmp + mv) so the reader never sees a partial file.
#
# THE BEAD CARRIES THE NUMBERS (law-escalations-carry-their-evidence). An Ops session that
# starts without context gathers the same data a few minutes later — describing a slightly
# different stall during an outage when the data is changing fastest.
# ---------------------------------------------------------------------------------------
if snapshot > "${PROMPT_FILE}.tmp" 2>/dev/null && mv -f "${PROMPT_FILE}.tmp" "$PROMPT_FILE"; then
    # THE MARKER ADVANCES ONLY HERE — after a successful write. A failed write leaves the
    # marker where it was so the next sweep sees the same records rather than losing them.
    printf '%s\n' "$_lapsed_now" > "$LAPSED_MARKER" 2>/dev/null || true
    log "watchtower: swept — ${since_land}m since the last landing, $(g SP_UNLANDED_N) unlanded, ${aeons_live} aeons, ${lapsed_count} lapsed"
else
    rm -f "${PROMPT_FILE}.tmp"
    log "watchtower: could not write the prompt file ($PROMPT_FILE)"
    exit 1
fi

# DRAIN ESCALATION. The prompt above already carries the drain state as a vital sign. When
# the drain has been armed longer than the threshold, file a dedicated bead so it reaches
# Ops even if the sweep itself is already open. Filed as P1 task, not a routine chore — a
# forgotten drain is a live condition that is starving the worker pool.
#
# ONLY WHEN DRAINING AND NUMERIC. A `?` drain_mins means the probe failed; filing an
# escalation on an unreadable probe would sound the alarm without evidence
# (law-absence-needs-a-positive-control). The halt guard above already exited when halted,
# so this branch only runs when the world is still moving.
if [ -n "$drain_since" ] && [ "$drain_mins" != "?" ] && \
   [ "$drain_mins" -ge "$DRAIN_WARN_MINS" ] 2>/dev/null; then
    if [ -x "$INC" ] || [ -r "$INC" ]; then
        printf 'DRAINING for %sm — summons gated since %s\n\nNew aeons cannot be summoned while world.draining exists. Loop, landing and reaping continue.\n\nLift with: world.sh resume\n' \
            "$drain_mins" "$drain_since" | \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_INCIDENT_TYPE=task \
        SPIRA_INCIDENT_PRIORITY=1 \
        SPIRA_INCIDENT_ACTOR=watchtower \
        SPIRA_SIN_EXEMPT=1 \
        SPIRA_INCIDENT_REPO=spira \
        bash "$INC" file "DRAINING: world.sh summons gated" - >/dev/null || true
        log "watchtower: drain escalation filed (${drain_mins}m >= ${DRAIN_WARN_MINS}m threshold)"
    else
        log "watchtower: $INC is missing — drain escalation not filed"
    fi
fi

# ---------------------------------------------------------------------------------------
# SENDING ESCALATIONS. The snapshot already carries the Sending vital signs; these are the
# thresholds at which the sweep alone is not enough and a dedicated bead is warranted.
#
# OLDEST-UNSENT. An unsent branch belonging to a live in_progress bead is work in flight,
# not backlog — but a branch older than SPIRA_UNSENT_WARN_H hours without a matching open
# bead is a branch nobody is about to send, and the rite that should reap it has failed
# or not run. Filed only when SP_UNSENT_OLDEST_H is numeric and at or above the threshold.
#
# UNADOPTED. A spira/* branch whose suffix resolves to no bead AND whose commits are all
# already on the base branch is a true stray — the reaper checks the bead, finds nothing,
# and skips, so it is a permanent +1 until removed by hand. Filed whenever SP_UNADOPTED is
# nonzero, using incident.sh dedup so repeated sweeps bump a recurrence rather than filing
# duplicates. SP_ORPHAN_WORK (no bead but commits absent from base) is deliberately excluded:
# deleting orphan work would destroy unlanded commits, so it is not a reapable stray and must
# never be filed as one. (sp-doh5)
#
# ONLY WHEN NUMERIC. A `?` means the probe failed; filing an escalation on an unreadable
# probe would sound the alarm without evidence (law-absence-needs-a-positive-control).
_unsent_oldest="${SP_UNSENT_OLDEST_H:-?}"
_unadopted="${SP_UNADOPTED:-?}"

if [ "$_unsent_oldest" != "?" ] && [ "$_unsent_oldest" -ge "$UNSENT_WARN_H" ] 2>/dev/null; then
    if [ -x "$INC" ] || [ -r "$INC" ]; then
        printf 'Oldest unsent branch: %sh — threshold is %sh\n\nA branch this old without a landing means the Sending rite has not run or cannot delete it.\nBranches owned by live in_progress beads are work in flight; confirm the branch has no holder before acting.\n\nCheck sending.sh and the rite logs. Reap manually if the owning bead is already closed.\n' \
            "$_unsent_oldest" "$UNSENT_WARN_H" | \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_INCIDENT_TYPE=task \
        SPIRA_INCIDENT_PRIORITY=1 \
        SPIRA_INCIDENT_ACTOR=watchtower \
        SPIRA_SIN_EXEMPT=1 \
        SPIRA_INCIDENT_REPO=spira \
        SPIRA_INCIDENT_REF=incident:sending-oldest-unsent \
        SPIRA_INCIDENT_CAUSE=oldest-unsent \
        bash "$INC" file "SENDING: oldest unsent branch above threshold" - >/dev/null || true
        log "watchtower: sending escalation filed (oldest unsent ${_unsent_oldest}h >= ${UNSENT_WARN_H}h threshold)"
    else
        log "watchtower: $INC is missing — sending escalation not filed"
    fi
fi

if [ "$_unadopted" != "?" ] && [ "$_unadopted" -gt 0 ] 2>/dev/null; then
    if [ -x "$INC" ] || [ -r "$INC" ]; then
        printf 'Unadopted refs: %s\n\nA spira/* branch whose suffix resolves to no bead can never be reaped by any rite.\nEach one is a permanent +1 on SP_UNADOPTED until removed by hand.\n\nBranches (spira/ prefix omitted): %s\nDelete safely: git -C %s branch -D spira/<id> (no bead, no aeon holds it)\n' \
            "$_unadopted" "${SP_UNADOPTED_NAMES:-(unavailable)}" "$SPIRA_REPO" | \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_INCIDENT_TYPE=task \
        SPIRA_INCIDENT_PRIORITY=2 \
        SPIRA_INCIDENT_ACTOR=watchtower \
        SPIRA_SIN_EXEMPT=1 \
        SPIRA_INCIDENT_REPO=spira \
        SPIRA_INCIDENT_REF=incident:sending-unadopted-refs \
        SPIRA_INCIDENT_CAUSE=unadopted-refs \
        SPIRA_INCIDENT_DELIVERS=action \
        bash "$INC" file "SENDING: unadopted refs cannot be reaped" - >/dev/null || true
        log "watchtower: unadopted escalation filed (${_unadopted} unadopted refs)"
    else
        log "watchtower: $INC is missing — unadopted escalation not filed"
    fi
fi

# ---------------------------------------------------------------------------------------
# BATCHED-STRANDED ESCALATION. A branch whose landstate is BATCHED but whose ID is absent
# from every open batch members= line is permanently skipped by sending.sh (CERTIFIED/BATCHED
# guard). It will age in SP_UNSENT_OLDEST_H forever, and any watchtower that reads only the
# BATCHED state will say "normal queue" when the work is actually stranded.
#
# ONLY WHEN NUMERIC AND NONZERO. A `?` means cockpit.sh did not emit the key (older snapshot
# predating this probe); filing on an unread probe would alarm without evidence.
# ---------------------------------------------------------------------------------------
_batched_stranded="${SP_BATCHED_STRANDED:-?}"
if [ "$_batched_stranded" != "?" ] && [ "$_batched_stranded" -gt 0 ] 2>/dev/null; then
    if [ -x "$INC" ] || [ -r "$INC" ]; then
        printf 'Stranded BATCHED branches: %s\n\nThe branch(es) below have BATCHED landstate but their ID is absent from every open batch members= line. sending.sh refuses to reap BATCHED branches, so these are permanently stuck until the landstate is corrected.\n\nBranch IDs (spira/ prefix omitted): %s\n\nCheck: for each id, read $SPIRA_RUN/landstate/<id> (first field = BATCHED) and confirm the id does not appear in $SPIRA_QUEUE_DIR/*/open members= lines.\nFix: if the branch still points to the BATCHED tip, recertify: land_mark <id> CERTIFIED <tip>. If the tip moved, escalate — the branch has diverged from what was batched.\n' \
            "$_batched_stranded" "${SP_BATCHED_STRANDED_NAMES:-(unavailable)}" | \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_INCIDENT_TYPE=task \
        SPIRA_INCIDENT_PRIORITY=1 \
        SPIRA_INCIDENT_ACTOR=watchtower \
        SPIRA_SIN_EXEMPT=1 \
        SPIRA_INCIDENT_REPO=spira \
        SPIRA_INCIDENT_REF=incident:sending-batched-stranded \
        SPIRA_INCIDENT_CAUSE=batched-stranded \
        bash "$INC" file "SENDING: BATCHED branch absent from open batch" - >/dev/null || true
        log "watchtower: batched-stranded escalation filed (${_batched_stranded} stranded)"
    else
        log "watchtower: $INC is missing — batched-stranded escalation not filed"
    fi
fi

# ---------------------------------------------------------------------------------------
# BATCHED-TOO-LONG ESCALATION. A BATCHED landstate record older than one batch interval
# (SPIRA_QUEUE_BATCH_WAIT) means the batch has not resolved. The root cause is usually a
# conflicting PR that was not detected; batch.sh now checks mergeability on every pass and
# abandons DIRTY batches, so this fires only when that check itself fails.
# ---------------------------------------------------------------------------------------
_batched_too_long="${SP_BATCHED_TOO_LONG:-?}"
if [ "$_batched_too_long" != "?" ] && [ "$_batched_too_long" -gt 0 ] 2>/dev/null; then
    if [ -x "$INC" ] || [ -r "$INC" ]; then
        printf 'BATCHED branches not resolved after one batch interval: %s\n\nThe branch(es) below have been in BATCHED state longer than expected:\n\n%s\n\nCheck: is the open batch PR mergeable? Run: gh pr view <pr-number> --json mergeable,mergeStateStatus.\nFix: if DIRTY, abandon the batch: queue.sh abandon <repo> --reason "conflict".\n' \
            "$_batched_too_long" "${SP_BATCHED_TOO_LONG_NAMES:-(unavailable)}" | \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_INCIDENT_TYPE=task \
        SPIRA_INCIDENT_PRIORITY=1 \
        SPIRA_INCIDENT_ACTOR=watchtower \
        SPIRA_SIN_EXEMPT=1 \
        SPIRA_INCIDENT_REPO=spira \
        SPIRA_INCIDENT_REF=incident:queue-batched-too-long \
        SPIRA_INCIDENT_CAUSE=batched-too-long \
        bash "$INC" file "QUEUE: BATCHED branch not resolved (too long)" - >/dev/null || true
        log "watchtower: batched-too-long escalation filed (${_batched_too_long} branches)"
    else
        log "watchtower: $INC is missing — batched-too-long escalation not filed"
    fi
fi

# ---------------------------------------------------------------------------------------
# CLOSED-STRANDED ESCALATION. A branch whose bead is CLOSED but that sending.sh has not
# reaped (content_landed=false, no PR merged at tip, no landed() hit) accumulates silently.
# The pane now separates these from in-flight branches, so the number is visible; this
# escalation fires when the oldest has been waiting more than CLOSED_STRANDED_WARN_H hours,
# which means sending.sh has seen it many times and none of its rules matched.
#
# Filed only when numeric and above the threshold (a `?` means the snapshot predates this
# key and we have no evidence to act on).
# ---------------------------------------------------------------------------------------
_closed_stranded_oldest="${SP_CLOSED_STRANDED_OLDEST_H:-?}"
if [ "$_closed_stranded_oldest" != "?" ] && \
   [ "$_closed_stranded_oldest" -ge "$CLOSED_STRANDED_WARN_H" ] 2>/dev/null; then
    if [ -x "$INC" ] || [ -r "$INC" ]; then
        printf 'Closed-bead branches not reaped: oldest %sh (threshold %sh)\n\nThese branches belong to CLOSED beads but sending.sh has kept them every pass because none of its reap rules matched. Each pass makes a GitHub API call per branch and logs a KEEP line.\n\nRun: sending.sh --dry-run  to see each branch and the rule it failed.\n\nCommon causes:\n  - work landed via a batch PR whose commit names the bead (check: git log --grep=<id> origin/main)\n  - a superseded bead with an empty branch (check n>0 guard)\n  - a non-code deliverable bead with no delivers: label\n\nReap by hand if confirmed safe: spira_destroy_branch / spira_destroy_worktree via sending.sh one-shot.\n' \
            "$_closed_stranded_oldest" "$CLOSED_STRANDED_WARN_H" | \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_INCIDENT_TYPE=task \
        SPIRA_INCIDENT_PRIORITY=2 \
        SPIRA_INCIDENT_ACTOR=watchtower \
        SPIRA_SIN_EXEMPT=1 \
        SPIRA_INCIDENT_REPO=spira \
        SPIRA_INCIDENT_REF=incident:sending-closed-stranded \
        SPIRA_INCIDENT_CAUSE=closed-stranded \
        SPIRA_INCIDENT_DELIVERS=action \
        bash "$INC" file "SENDING: closed-bead branch not reaped above threshold" - >/dev/null || true
        log "watchtower: closed-stranded escalation filed (oldest ${_closed_stranded_oldest}h >= ${CLOSED_STRANDED_WARN_H}h)"
    else
        log "watchtower: $INC is missing — closed-stranded escalation not filed"
    fi
fi

# ---------------------------------------------------------------------------------------
# DUPLICATE-REF ESCALATION. The dedup meter above measures whether incident.sh is actually
# deduping. When SP_DUP_REFS is nonzero, multiple beads carry the same external_ref — which
# means the dedup path silently stopped working at some point, and every subsequent filing
# piled on a fresh bead rather than bumping a recurrence. Filed as a P1 task to Ops; the
# body names the worst offending refs so Ops can immediately see what to collapse.
#
# DEDUP THROUGH THE FIXED PATH (sp-srgr6). The SPIRA_INCIDENT_REF is a stable key, not one
# that embeds the current count — a count-keyed ref produces a new bead on every measurement,
# which is the exact failure this escalation exists to catch.
#
# ONLY WHEN NUMERIC AND NONZERO. A `?` means the probe failed; filing on a failed probe
# sounds the alarm without evidence (law-absence-needs-a-positive-control). Zero is the
# healthy state and is never filed.
# ---------------------------------------------------------------------------------------
_dup_refs="${SP_DUP_REFS:-?}"
_dup_beads="${SP_DUP_BEADS:-?}"
if [ "$_dup_refs" != "?" ] && [ "$_dup_refs" -gt 0 ] 2>/dev/null; then
    if [ -x "$INC" ] || [ -r "$INC" ]; then
        # Build the body: worst offenders from SP_DUP_ROWn, falling back to bare counts.
        _dup_body="$(printf 'Duplicate incident refs: %s refs, %s surplus beads\n\nThe incident.sh dedup path is not deduplicating within its lookback window. Multiple beads exist for the same external_ref, which means each pass filed a fresh bead instead of bumping a recurrence. Collapse the surplus beads and investigate why open_incident() or recent_closed_incident() missed the existing one.\n\nWorst offenders:\n' "$_dup_refs" "$_dup_beads"
        for _i in 0 1 2 3 4; do
            _row_var="SP_DUP_ROW${_i}"
            _row="${!_row_var:-}"
            [ -n "$_row" ] && printf '  %s\n' "$_row"
        done)"
        printf '%s\n' "$_dup_body" | \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_INCIDENT_TYPE=task \
        SPIRA_INCIDENT_PRIORITY=1 \
        SPIRA_INCIDENT_ACTOR=watchtower \
        SPIRA_SIN_EXEMPT=1 \
        SPIRA_INCIDENT_REPO=spira \
        SPIRA_INCIDENT_REF=incident:dedup-meter-nonzero \
        SPIRA_INCIDENT_CAUSE=dedup-meter \
        bash "$INC" file "DEDUP: duplicate incident refs detected (${_dup_refs} refs, ${_dup_beads} surplus)" - >/dev/null || true
        log "watchtower: dedup escalation filed (${_dup_refs} dup refs, ${_dup_beads} surplus beads)"
    else
        log "watchtower: $INC is missing — dedup escalation not filed"
    fi
fi

# ---------------------------------------------------------------------------------------
# MOOT-ASK SWEEP. Auto-filed asks record the condition that fired them as a MOOT-WHEN:
# command in their description. When that command exits 0, the condition has cleared and
# the ask is no longer actionable — resolve it so it does not consume the operator's
# attention on every session start. The sweep runs here on the same cadence because these
# conditions are the same ones this pass already reads (law-detection-outranks-rejection).
# ---------------------------------------------------------------------------------------
MOOT_SH="${SPIRA_MOOT_SH:-$(dirname "$0")/../cockpit/moot-sweep.sh}"
if [ -r "$MOOT_SH" ]; then
    bash "$MOOT_SH" --apply >/dev/null 2>&1 || \
        log "watchtower: moot-sweep exited non-zero — check $MOOT_SH"
    log "watchtower: moot-sweep ran"
else
    log "watchtower: moot-sweep skipped — $MOOT_SH is missing or unreadable"
fi
