#!/usr/bin/env bash
# queue.sh — submit a branch into the merge queue; protect the base branch; report stats.
#
#   queue.sh submit <branch> [<repo>]
#   queue.sh protect [<repo>]
#   queue.sh stats
#   queue.sh flush [<repo>]
#   queue.sh step <repo>
#   queue.sh abandon [<repo>] --reason <text> [--dry-run]
#
# submit: certifies any branch by running the repository's gate. In a queue-mode
# repository, a green branch is recorded CERTIFIED for the batch builder.
# In push mode the branch is fast-forward merged to the base; in pr or
# hold mode it is certified and left for landing.sh. A branch with no
# associated bead may be submitted.
#
# protect: sets the required gate check, disallows force-push and deletion on the
# queue-mode repo's base branch via the forge seam (SPIRA_FORGE), then writes a
# receipt under SPIRA_RUN that doctor.sh checks. Defaults to the home repo.
#
# flush: opens a batch now from whatever is CERTIFIED, instead of waiting for
# SPIRA_QUEUE_BATCH_WAIT or SPIRA_QUEUE_BATCH_MAX. The batch builder's own rules
# otherwise hold: one open batch per repository, conflicts skipped.
# step: one landing pass over a queue-mode repository — settle the open batch from its
# CI result, then open the next one if due. Settling first is what lets a landed batch be
# followed by a new one in the same pass.
# stats: reads QUEUE lines from landing.log and prints caught/escaped/cost totals.
# abandon: closes the open batch PR, returns innocent members to CERTIFIED, archives
# the record. RED and EJECTED members are left alone — abandon never re-certifies a
# branch someone deliberately ejected. Break-glass: --reason is required, and one
# QUEUE ABANDON line (landing.log) plus a queue.abandoned event names the actor, the
# PR and every member's disposition.
#
# covers: spira/queue.sh spira/suites.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/lib.sh"


cmd_submit() {
    local br="${1:-}"
    [ -n "$br" ] || { printf 'queue.sh submit: branch name required\n' >&2; return 2; }

    local repo name mode
    name="${2:-$(spira_home_repo)}"
    repo="$(repo_root "$name" 2>/dev/null)" || repo="$SPIRA_REPO"
    mode="$(repo_land "$name")"

    if [ "$mode" = "queue" ]; then
        case "$br" in
        spira/*|spira-suite-state/*) ;;
        *)
            printf 'queue.sh submit: %s: queue mode requires a branch under spira/ or spira-suite-state/\n' "$br" >&2
            return 1
            ;;
        esac
    fi

    git -C "$repo" show-ref --verify --quiet "refs/heads/$br" 2>/dev/null || {
        printf 'queue.sh submit: branch not found: %s\n' "$br" >&2
        return 1
    }

    local tip id
    tip="$(git -C "$repo" rev-parse "$br" 2>/dev/null)"
    id="${br#spira/}"

    # Certify: run the gate.
    local gate_out gate_rc gate_start
    gate_start="$(date +%s)"
    gate_out="$(SPIRA_GATE_BEAD="$id" "$HERE/gate.sh" "$br" "$name" 2>&1)"
    gate_rc=$?

    local gate_outcome
    gate_outcome="$(spira_gate_outcome "$gate_rc")"

    if [ "$gate_rc" -ne 0 ]; then
        printf 'queue.sh submit: %s failed the gate (%s)\n' "$br" "$gate_outcome" >&2
        printf '%s\n' "$gate_out" >&2
        printf 'QUEUE CAUGHT %s branch=%s\n' "$(date +%s)" "$id" \
            >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
        return "$gate_rc"
    fi

    local gate_cost=$(( $(date +%s) - gate_start ))
    printf 'QUEUE GATE_COST %s branch=%s seconds=%d\n' "$(date +%s)" "$id" "$gate_cost" \
        >> "$SPIRA_RUN/landing.log" 2>/dev/null || true

    case "$mode" in
    queue)
        land_mark "$id" CERTIFIED "$tip"
        mkdir -p "$(dirname "$SPIRA_QUEUE_DIR/$id")" 2>/dev/null
        printf 'CERTIFIED %s %s\n' "$tip" "$(date +%s)" > "$SPIRA_QUEUE_DIR/$id" || {
            printf 'queue.sh submit: failed to write queue entry for %s\n' "$br" >&2
            return 1
        }
        printf 'queue.sh submit: certified %s\n' "$br"
        ;;
    push)
        local base base_remote base_branch
        base="$(spira_landref "$repo")" || {
            printf 'queue.sh submit: cannot resolve landing ref for %s\n' "$name" >&2
            return 1
        }
        base_remote="$(ref_remote "$base")" || base_remote=""
        base_branch="$(ref_branch "$base")"
        [ -n "$base_remote" ] && git -C "$repo" fetch -q "$base_remote" 2>/dev/null || true
        rebase_branch "$br" "$base" "$repo" "$name" 2>/dev/null || {
            printf 'queue.sh submit: %s does not rebase onto %s (%s)\n' \
                "$br" "$base" "${REBASE_FAILURE:-conflict}" >&2
            return 1
        }
        tip="$(git -C "$repo" rev-parse "$br" 2>/dev/null)"
        spira_git_push "$repo" -q "${base_remote:-origin}" "$br:$base_branch" 2>/dev/null || {
            printf 'queue.sh submit: push of %s to %s failed\n' "$br" "$base_branch" >&2
            return 1
        }
        land_mark "$id" LANDED "$tip"
        bead_close_on_land "$id" "$tip" || true
        printf 'queue.sh submit: landed %s (push)\n' "$br"
        ;;
    pr|hold)
        land_mark "$id" CERTIFIED "$tip"
        printf 'queue.sh submit: certified %s (%s)\n' "$br" "$mode"
        ;;
    esac
}

cmd_stats() {
    local log="$SPIRA_RUN/landing.log"
    local caught=0 escaped=0 batches=0 total_members=0 total_cost=0
    local local_batches=0 local_red=0

    if [ -r "$log" ]; then
        while IFS= read -r _line; do
            case "$_line" in
                "QUEUE CAUGHT "*)
                    caught=$(( caught + 1 ))
                    ;;
                "QUEUE ESCAPED "*)
                    escaped=$(( escaped + 1 ))
                    ;;
                "QUEUE BATCH "*)
                    batches=$(( batches + 1 ))
                    local _m _c _v _gs
                    _m="$(printf '%s' "$_line" | sed 's/.*members=\([0-9]*\).*/\1/')"
                    _v="$(printf '%s' "$_line" | sed -n 's/.*verdict=\([a-z]*\).*/\1/p')"
                    _gs="$(printf '%s' "$_line" | sed -n 's/.*gate_seconds=\([0-9]*\).*/\1/p')"
                    _c="$(printf '%s' "$_line" | sed -n 's/.*cost=\([0-9]*\)s.*/\1/p')"
                    case "${_m:-}" in ''|*[!0-9]*) _m=0 ;; esac
                    case "${_gs:-}" in ''|*[!0-9]*) _gs=0 ;; esac
                    case "${_c:-}" in ''|*[!0-9]*) _c=0 ;; esac
                    total_members=$(( total_members + _m ))
                    # Local-gate lines carry verdict=; CI lines carry cost=.
                    if [ -n "${_v:-}" ]; then
                        local_batches=$(( local_batches + 1 ))
                        [ "$_v" = red ] && local_red=$(( local_red + 1 ))
                        total_cost=$(( total_cost + _gs ))
                    else
                        total_cost=$(( total_cost + _c ))
                    fi
                    ;;
                "QUEUE GATE_COST "*)
                    local _s
                    _s="$(printf '%s' "$_line" | sed 's/.*seconds=\([0-9]*\).*/\1/')"
                    case "${_s:-}" in ''|*[!0-9]*) _s=0 ;; esac
                    total_cost=$(( total_cost + _s ))
                    ;;
            esac
        done < "$log"
    fi

    local avg_cost=0
    [ "$total_members" -gt 0 ] && avg_cost=$(( total_cost / total_members ))

    local red_pct=0
    [ "$local_batches" -gt 0 ] && red_pct=$(( local_red * 100 / local_batches ))

    printf 'caught:          %d\n' "$caught"
    printf 'escaped:         %d\n' "$escaped"
    printf 'batches:         %d (%d members)\n' "$batches" "$total_members"
    printf 'local_red_rate:  %d/%d (%d%%)\n' "$local_red" "$local_batches" "$red_pct"
    printf 'cost:            %ds avg per branch\n' "$avg_cost"
}

cmd_protect() {
    local name="${1:-}"
    [ -n "$name" ] || name="$(spira_home_repo)"

    local repo mode base base_branch
    repo="$(repo_root "$name" 2>/dev/null)" || {
        printf 'queue.sh protect: no such repo: %s\n' "$name" >&2; return 1
    }
    mode="$(repo_land "$name")"
    [ "$mode" = queue ] || {
        printf 'queue.sh protect: repo is not in queue mode (mode=%s)\n' "$mode" >&2; return 1
    }

    base="$(spira_landref "$name" 2>/dev/null)" || {
        printf 'queue.sh protect: cannot resolve base ref for %s\n' "$name" >&2; return 1
    }
    base_branch="$(ref_branch "$base")"

    "$SPIRA_FORGE" branch-protect "$repo" "$base_branch" || {
        printf 'queue.sh protect: forge branch-protect failed\n' >&2; return 1
    }

    local receipt="$SPIRA_RUN/queue-protected-$name"
    mkdir -p "$SPIRA_RUN" 2>/dev/null || true
    printf '%s\n' "$base_branch" > "$receipt"
    printf 'queue.sh protect: protection set for repo:%s (branch: %s)\n' "$name" "$base_branch"
    printf 'queue.sh protect: attribution note: suite-level attribution requires CI to emit\n'
    printf 'queue.sh protect:   failure annotations whose path matches spira/test-*.sh,\n'
    printf 'queue.sh protect:   or a check annotation titled "red-twice suite".\n'
    printf 'queue.sh protect:   Without them the queue uses bisect attribution (O(log n) CI runs).\n'
}

# _batch_cut <repo-name> — the merge-queue's round cutter. Delegates to the batcher crate
# (sp-jzfog) when SPIRA_QUEUE_BATCHER=1 and its binary is built; batch.sh's own inline cut
# otherwise. Off by default — sp-vsob2 retires batch.sh's cut and flips this once the
# batcher has proven itself live, which is a deliberate cutover, not this flag's default.
_batch_cut() {
    if [ "${SPIRA_QUEUE_BATCHER:-0}" = 1 ] && [ -x "${SPIRA_BATCHER_BIN:-}" ]; then
        "$SPIRA_BATCHER_BIN" cut "$1"
        return $?
    fi
    bash "$HERE/batch.sh" "$1"
}

cmd_flush() {
    local name="${1:-}"
    [ -n "$name" ] || name="$(spira_home_repo)"
    repo_root "$name" >/dev/null 2>&1 || {
        printf 'queue.sh flush: no such repo: %s\n' "$name" >&2; return 1
    }
    local mode; mode="$(repo_land "$name")"
    [ "$mode" = queue ] || {
        printf 'queue.sh flush: repo is not in queue mode (mode=%s)\n' "$mode" >&2; return 1
    }
    SPIRA_QUEUE_BATCH_WAIT=0 _batch_cut "$name"
}

cmd_step() {
    local name="${1:?queue.sh step: repo required}"
    bash "$HERE/verdict.sh" "$name"
    _batch_cut "$name"
}

cmd_eject() {
    local id="${1:-}" reason="" dry_run=0 name=""
    [ -n "$id" ] || { printf 'queue.sh eject: bead id required\n' >&2; return 2; }
    if [ "${SPIRA_FAYTH:-}" = czar ] && [ -n "${SPIRA_CZAR_CLASS:-}" ]; then
        bash "$HERE/czar-fence.sh" "$SPIRA_CZAR_CLASS" || return 1
    fi
    shift

    while [ $# -gt 0 ]; do
        case "$1" in
        --reason)   shift; reason="${1:-}"; shift ;;
        --reason=*) reason="${1#--reason=}"; shift ;;
        --dry-run)  dry_run=1; shift ;;
        -*)         printf 'queue.sh eject: unknown option: %s\n' "$1" >&2; return 2 ;;
        *)          name="$1"; shift ;;
        esac
    done

    [ -n "$name" ] || name="$(spira_home_repo)"
    repo_root "$name" >/dev/null 2>&1 || {
        printf 'queue.sh eject: no such repo: %s\n' "$name" >&2; return 1
    }

    # Take the per-repo lock — eject racing a batch build produces the
    # two-PRs-one-batch state the lock exists to prevent. Held for both paths below:
    # a batch build can race either an ejection from the open batch or a withdrawal
    # of a certified-but-unbatched bead.
    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    { exec 9>"$lockfile"; } 2>/dev/null \
        || { printf 'queue.sh eject: cannot open lock file for %s\n' "$name" >&2; return 1; }
    if ! flock -n 9; then
        printf 'queue.sh eject: another queue operation holds the lock for %s\n' "$name" >&2
        return 1
    fi

    local open_file="${SPIRA_QUEUE_DIR:?}/$name/open"
    local members_val="" pr_n="" tip="" found=0 new_members=""
    if [ -f "$open_file" ]; then
        members_val="$(grep '^members=' "$open_file" | head -1)"
        members_val="${members_val#members=}"
        pr_n="$(grep '^pr=' "$open_file" | head -1)"; pr_n="${pr_n#pr=}"

        local _m mid mtip
        for _m in $members_val; do
            mid="${_m%%:*}"; mtip="${_m##*:}"
            if [ "$mid" = "$id" ]; then
                found=1; tip="$mtip"
            else
                new_members="${new_members}${new_members:+ }$_m"
            fi
        done
    fi

    if [ "$found" -eq 0 ]; then
        # Not a member of any open batch (or none is open) — a CERTIFIED bead that
        # has not yet been picked up by a batch build is still withdrawable, through
        # the same mechanism a reopen uses (bead_reopen clears CERTIFIED to WITHDRAWN).
        local _cert_st _cert_tip
        read -r _cert_st _cert_tip _ <<< "$(land_state "$id" 2>/dev/null)"

        if [ "${_cert_st:-}" != CERTIFIED ]; then
            printf 'queue.sh eject: %s is not a member of the open batch for %s and is not CERTIFIED\n' "$id" "$name" >&2
            local _ids=""
            for _m in $members_val; do _ids="${_ids}${_ids:+ }${_m%%:*}"; done
            printf 'batch members: %s\n' "${_ids:-<none>}" >&2
            return 1
        fi

        if [ "$dry_run" -eq 1 ]; then
            printf 'dry-run: %s is CERTIFIED but not yet batched for %s (tip=%s)\n' "$id" "$name" "$_cert_tip"
            printf 'dry-run: would write WITHDRAWN to %s/%s\n' "$LANDSTATE" "$id"
            printf 'dry-run: would reopen bead %s and clear assignee\n' "$id"
            printf 'dry-run: would post comment to %s\n' "$id"
            bdq show "$id" >/dev/null 2>&1 || {
                printf 'dry-run: ERROR: cannot resolve bead %s\n' "$id" >&2; return 1
            }
            return 0
        fi

        bead_reopen "$id" "eject"

        local _comment
        _comment="Ejected while certified but not yet batched in $name.${reason:+$'\n\n'${reason}}"$'\n\n'"Landstate written as WITHDRAWN. Recertify the branch before it can rejoin the queue."
        printf '%s' "$_comment" | bdq comment "$id" --stdin >/dev/null 2>&1 || true

        printf 'queue.sh eject: ejected %s (certified, not yet batched) for %s (landstate=WITHDRAWN)\n' "$id" "$name"
        return 0
    fi

    if [ "$dry_run" -eq 1 ]; then
        printf 'dry-run: %s is in the open batch for %s (tip=%s)\n' "$id" "$name" "$tip"
        printf 'dry-run: would write RED to %s/%s\n' "$LANDSTATE" "$id"
        printf 'dry-run: would reopen bead %s and clear assignee\n' "$id"
        printf 'dry-run: would post comment to %s\n' "$id"
        printf 'dry-run: would close PR %s\n' "$pr_n"
        if [ -n "$new_members" ]; then
            local _surv=""
            for _m in $new_members; do _surv="${_surv}${_surv:+ }${_m%%:*}"; done
            printf 'dry-run: would return survivors to CERTIFIED: %s\n' "$_surv"
        fi
        bdq show "$id" >/dev/null 2>&1 || {
            printf 'dry-run: ERROR: cannot resolve bead %s\n' "$id" >&2; return 1
        }
        return 0
    fi

    # Use RED, not EJECTED: EJECTED is the automated local-gate attribution state (batch.sh);
    # RED is the operator's verdict on a manually identified failure.
    land_mark "$id" RED "$tip" "${reason:-ejected}"

    bead_reopen "$id" "eject"
    # A submitted bead is excluded from ready (fayth_exclude) so it is not reclaimed
    # mid-flight; an ejected bead must be claimable again, so the label goes with it.
    bdq label remove "$id" "${SPIRA_SUBMITTED_LABEL:-spira-submitted}" >/dev/null 2>&1 || true

    local _comment
    _comment="Ejected from open batch in $name.${reason:+$'\n\n'${reason}}"$'\n\n'"Landstate written as RED. Fix the failing issue and re-certify before rejoining the queue."
    printf '%s' "$_comment" | bdq comment "$id" --stdin >/dev/null 2>&1 || true

    # Close the PR and return survivors to CERTIFIED. The batch branch contains
    # the ejected member's commits and is stale; a fresh batch rebuilds from CERTIFIED.
    local forge="${SPIRA_FORGE:-$HERE/forge.sh}"
    local repo_dir; repo_dir="$(repo_root "$name")"
    for _m in $new_members; do
        mid="${_m%%:*}"; mtip="${_m##*:}"
        land_mark "$mid" CERTIFIED "$mtip"
        printf 'queue.sh eject: %s returned to CERTIFIED\n' "$mid"
    done
    "$forge" pr-close "$repo_dir" "$pr_n" 2>/dev/null || true
    rm -f "$open_file"

    printf 'queue.sh eject: ejected %s from %s batch (landstate=RED)\n' "$id" "$name"
}

cmd_abandon() {
    local name="" reason="" dry_run=0
    if [ "${SPIRA_FAYTH:-}" = czar ] && [ -n "${SPIRA_CZAR_CLASS:-}" ]; then
        bash "$HERE/czar-fence.sh" "$SPIRA_CZAR_CLASS" || return 1
    fi

    while [ $# -gt 0 ]; do
        case "$1" in
        --reason)   shift; reason="${1:-}"; shift ;;
        --reason=*) reason="${1#--reason=}"; shift ;;
        --dry-run)  dry_run=1; shift ;;
        -*)         printf 'queue.sh abandon: unknown option: %s\n' "$1" >&2; return 2 ;;
        *)          name="$1"; shift ;;
        esac
    done

    # Break-glass with no glass is a quiet door: refuse without a stated cause, and
    # name the sanctioned way through (law-a-refusal-must-name-its-own-authorised-exit,
    # proposed but not yet in force).
    [ -n "$reason" ] || {
        printf 'queue.sh abandon: --reason is required — pass --reason "<why>" so the abandon is auditable\n' >&2
        return 2
    }

    local actor; actor="${BEADS_ACTOR:-${SPIRA_AEON:+aeon-$SPIRA_AEON}}"; actor="${actor:-${USER:-unknown}}"

    [ -n "$name" ] || name="$(spira_home_repo)"
    repo_root "$name" >/dev/null 2>&1 || {
        printf 'queue.sh abandon: no such repo: %s\n' "$name" >&2; return 1
    }

    # Take the per-repo lock — abandon racing a batch build produces the
    # two-PRs-one-batch state that sp-qdtnw exists to prevent.
    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    { exec 9>"$lockfile"; } 2>/dev/null \
        || { printf 'queue.sh abandon: cannot open lock file for %s\n' "$name" >&2; return 1; }
    if ! flock -n 9; then
        printf 'queue.sh abandon: another queue operation holds the lock for %s\n' "$name" >&2
        return 1
    fi

    local open_file="${SPIRA_QUEUE_DIR:?}/$name/open"
    if [ ! -f "$open_file" ]; then
        printf 'queue.sh abandon: no open batch for %s\n' "$name" >&2
        return 1
    fi

    local pr_n members_val branch_val
    pr_n="$(grep '^pr=' "$open_file" | head -1)"; pr_n="${pr_n#pr=}"
    members_val="$(grep '^members=' "$open_file" | head -1)"; members_val="${members_val#members=}"
    branch_val="$(grep '^branch=' "$open_file" | head -1)"; branch_val="${branch_val#branch=}"

    local stamp; stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    local archive_name="closed-pr${pr_n}-${stamp}"
    local archive_path="${SPIRA_QUEUE_DIR:?}/$name/$archive_name"

    if [ "$dry_run" -eq 1 ]; then
        printf 'dry-run: would close PR %s for %s\n' "$pr_n" "$name"
        local _m mid mtip cur_state member_audit=""
        for _m in $members_val; do
            mid="${_m%%:*}"; mtip="${_m##*:}"
            cur_state=""
            [ -f "$LANDSTATE/$mid" ] && { read -r cur_state _ < "$LANDSTATE/$mid" 2>/dev/null || true; }
            case "${cur_state:-}" in
            RED|EJECTED)
                printf 'dry-run: %s: leave alone (%s)\n' "$mid" "$cur_state"
                member_audit="${member_audit:+$member_audit,}${mid}:${cur_state}"
                ;;
            *)
                printf 'dry-run: %s: return to CERTIFIED at %s\n' "$mid" "$mtip"
                member_audit="${member_audit:+$member_audit,}${mid}:CERTIFIED"
                ;;
            esac
        done
        printf 'dry-run: would cancel non-completed Gate run(s) for branch %s\n' "$branch_val"
        printf 'dry-run: archive path: %s\n' "$archive_path"
        printf 'dry-run: audit line (landing.log): QUEUE ABANDON %s repo=%s pr=%s actor=%s members=%s reason=%s\n' \
            "$(date +%s)" "$name" "$pr_n" "$actor" "${member_audit:-<none>}" "${reason//$'\n'/ }"
        return 0
    fi

    local forge="${SPIRA_FORGE:-$HERE/forge.sh}"
    local repo_dir; repo_dir="$(repo_root "$name")"
    local comment_text="Batch abandoned.${reason:+ Reason: ${reason}}"
    queue_cancel_branch_runs "$forge" "$repo_dir" "$branch_val" "QUEUE" || true
    "$forge" pr-comment "$repo_dir" "$pr_n" "$comment_text" 2>/dev/null || true
    "$forge" pr-close   "$repo_dir" "$pr_n" 2>/dev/null || true

    # Return each member to CERTIFIED unless it was deliberately ejected.
    # RED and EJECTED are operator/batch verdicts that must survive an abandon.
    local _m mid mtip cur_state member_audit=""
    for _m in $members_val; do
        mid="${_m%%:*}"; mtip="${_m##*:}"
        cur_state=""
        [ -f "$LANDSTATE/$mid" ] && { read -r cur_state _ < "$LANDSTATE/$mid" 2>/dev/null || true; }
        case "${cur_state:-}" in
        RED|EJECTED)
            printf 'queue.sh abandon: %s: left as %s\n' "$mid" "$cur_state"
            member_audit="${member_audit:+$member_audit,}${mid}:${cur_state}"
            ;;
        *)
            land_mark "$mid" CERTIFIED "$mtip"
            printf 'queue.sh abandon: %s: returned to CERTIFIED\n' "$mid"
            member_audit="${member_audit:+$member_audit,}${mid}:CERTIFIED"
            ;;
        esac
    done

    local reason_clean="${reason//$'\n'/ }"

    # The archived record is the one place this survives once the PR is closed and its
    # comment is unreadable history — keep the actor and reason IN the file, not only there.
    {
        printf 'reason=%s\n' "$reason_clean"
        printf 'actor=%s\n' "$actor"
    } >> "$open_file"

    mv "$open_file" "$archive_path" || {
        printf 'queue.sh abandon: failed to archive open record\n' >&2; return 1
    }

    local audit_epoch; audit_epoch="$(date +%s)"
    printf 'QUEUE ABANDON %s repo=%s pr=%s actor=%s members=%s reason=%s\n' \
        "$audit_epoch" "$name" "$pr_n" "$actor" "${member_audit:-<none>}" "$reason_clean" \
        >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
    spira_event queue.abandoned - "abandoned PR $pr_n for $name (actor=$actor)" \
        "members=${member_audit:-<none>} reason=$reason_clean" || true

    printf 'queue.sh abandon: PR %s closed, batch abandoned for %s\n' "$pr_n" "$name"
}

case "${1:-}" in
    submit)  shift; cmd_submit "$@" ;;
    protect) shift; cmd_protect "$@" ;;
    stats)   cmd_stats ;;
    flush)   shift; cmd_flush "$@" ;;
    step)    shift; cmd_step "$@" ;;
    eject)   shift; cmd_eject "$@" ;;
    abandon) shift; cmd_abandon "$@" ;;
    *) printf 'usage: queue.sh submit <branch> [<repo>] | queue.sh protect [<repo>] | queue.sh stats | queue.sh flush [<repo>] | queue.sh step <repo> | queue.sh eject <id> [--reason <text>] [--dry-run] [<repo>] | queue.sh abandon [<repo>] --reason <text> [--dry-run]\n' >&2; exit 2 ;;
esac
