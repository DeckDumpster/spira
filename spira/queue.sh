#!/usr/bin/env bash
# queue.sh — submit a branch into the merge queue; protect the base branch; report stats.
#
#   queue.sh submit <branch> [<repo>]
#   queue.sh protect [<repo>]
#   queue.sh stats
#   queue.sh flush [<repo>]
#   queue.sh step <repo>
#   queue.sh abandon [<repo>] --reason <text> [--dry-run]
#   queue.sh open-batch [<repo>] [--members <ids>] [--skip-pregate] [--dry-run]
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
# open-batch: assembles a batch, pushes it, opens the PR and writes the open record — the
# hand tool for when the automatic cut cannot run. Selects from CERTIFIED with the same
# queue_sort_rows order batch.sh uses; --members overrides the selection with an explicit,
# space- or comma-separated list. A member that conflicts with the base or with the batch
# already assembled is skipped, not reopened, and the reason is printed. --skip-pregate
# opens the PR without the local pre-flight gate, leaving CI as the sole authority
# (law-local-gates-buy-latency-not-coverage); it is never the default, and the PR body says
# so when it is used. --dry-run reports the members, the skips, the merge head and the exact
# open record that would be written, without creating a branch, pushing, or opening a PR.
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

    # Certify: run the gate. SPIRA_CERTIFY_SUITES=off keeps the fences and drops the suites,
    # exactly as landing.sh's certification does. Without it this path — every aeon's own
    # teardown self-certification (sp-u9f82) — ran the touched-suite selection on branch and
    # base, 30-47 minutes per bead with the aeon holding its fleet slot throughout.
    local gate_out gate_rc gate_start
    gate_start="$(date +%s)"
    gate_out="$(SPIRA_GATE_BEAD="$id" SPIRA_GATE_SUITES="${SPIRA_CERTIFY_SUITES:-on}" \
        "$HERE/gate.sh" "$br" "$name" 2>&1)"
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
    local id="${1:-}" reason="" dry_run=0 name="" suites=""
    [ -n "$id" ] || { printf 'queue.sh eject: bead id required\n' >&2; return 2; }
    if [ "${SPIRA_FAYTH:-}" = czar ] && [ -n "${SPIRA_CZAR_CLASS:-}" ]; then
        bash "$HERE/czar-fence.sh" "$SPIRA_CZAR_CLASS" || return 1
    fi
    shift

    while [ $# -gt 0 ]; do
        case "$1" in
        --reason)   shift; reason="${1:-}"; shift ;;
        --reason=*) reason="${1#--reason=}"; shift ;;
        --suites)   shift; suites="${1:-}"; shift ;;
        --suites=*) suites="${1#--suites=}"; shift ;;
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
            [ -n "$suites" ] && printf 'dry-run: would write suites=%s to %s/%s.ejected\n' "$suites" "$LANDSTATE" "$id"
            printf 'dry-run: would reopen bead %s and clear assignee\n' "$id"
            printf 'dry-run: would post comment to %s\n' "$id"
            bdq show "$id" >/dev/null 2>&1 || {
                printf 'dry-run: ERROR: cannot resolve bead %s\n' "$id" >&2; return 1
            }
            return 0
        fi

        bead_reopen "$id" "eject" "" "$suites"

        local _comment
        _comment="Ejected while certified but not yet batched in $name.${reason:+$'\n\n'${reason}}"$'\n\n'"Landstate written as WITHDRAWN. Recertify the branch before it can rejoin the queue."
        [ -n "$suites" ] && _comment="$_comment"$'\n\n'"Recertification will force these suites regardless of SPIRA_CERTIFY_SUITES: $suites"
        printf '%s' "$_comment" | bdq comment "$id" --stdin >/dev/null 2>&1 || true

        printf 'queue.sh eject: ejected %s (certified, not yet batched) for %s (landstate=WITHDRAWN)\n' "$id" "$name"
        return 0
    fi

    if [ "$dry_run" -eq 1 ]; then
        printf 'dry-run: %s is in the open batch for %s (tip=%s)\n' "$id" "$name" "$tip"
        printf 'dry-run: would write RED to %s/%s\n' "$LANDSTATE" "$id"
        [ -n "$suites" ] && printf 'dry-run: would write suites=%s to %s/%s.ejected\n' "$suites" "$LANDSTATE" "$id"
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

    bead_reopen "$id" "eject" "" "$suites"
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

cmd_open_batch() {
    # Sourced here, not at file scope: a suite that replaces batch.sh with a bare spy
    # script (to watch how _batch_cut invokes it) must not have that spy's top-level
    # code executed just because queue.sh loaded — only open-batch needs its assembly
    # primitives (_base_conflict, format_batch, _batch_open_file, _batch_is_open, _pf_gate).
    . "$HERE/batch.sh"

    local name="" members_arg="" skip_pregate=0 dry_run=0
    if [ "${SPIRA_FAYTH:-}" = czar ] && [ -n "${SPIRA_CZAR_CLASS:-}" ]; then
        bash "$HERE/czar-fence.sh" "$SPIRA_CZAR_CLASS" || return 1
    fi

    while [ $# -gt 0 ]; do
        case "$1" in
        --members)      shift; members_arg="${1:-}"; shift ;;
        --members=*)    members_arg="${1#--members=}"; shift ;;
        --skip-pregate) skip_pregate=1; shift ;;
        --dry-run)      dry_run=1; shift ;;
        -*)  printf 'queue.sh open-batch: unknown option: %s\n' "$1" >&2; return 2 ;;
        *)   name="$1"; shift ;;
        esac
    done
    [ -n "$name" ] || name="$(spira_home_repo)"

    local repo; repo="$(repo_root "$name" 2>/dev/null)" || {
        printf 'queue.sh open-batch: no such repo: %s\n' "$name" >&2; return 1
    }
    local mode; mode="$(repo_land "$name")"
    [ "$mode" = queue ] || {
        printf 'queue.sh open-batch: repo is not in queue mode (mode=%s)\n' "$mode" >&2; return 1
    }

    # Take the sp-qdtnw lock — open-batch racing the landing pass's own cut produces
    # the two-PRs-one-batch state that lock exists to prevent.
    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    { exec 9>"$lockfile"; } 2>/dev/null \
        || { printf 'queue.sh open-batch: cannot open lock file for %s\n' "$name" >&2; return 1; }
    if ! flock -n 9; then
        printf 'queue.sh open-batch: another queue operation holds the lock for %s\n' "$name" >&2
        return 1
    fi

    if _batch_is_open "$name"; then
        printf 'queue.sh open-batch: a batch is already open for %s\n' "$name" >&2
        return 1
    fi

    local base base_sha remote base_branch
    base="$(spira_landref "$repo")" || {
        printf 'queue.sh open-batch: cannot resolve landing ref for %s\n' "$name" >&2; return 1
    }
    base_sha="$(git -C "$repo" rev-parse "$base" 2>/dev/null)" || {
        printf 'queue.sh open-batch: cannot resolve %s\n' "$base" >&2; return 1
    }
    remote="$(ref_remote "$base")"
    base_branch="$(ref_branch "$base")"

    local certs; certs="$(queue_certified_list "$repo")"

    # Candidate rows ("<id> <tip>"), in the order they will be tried.
    local candfile admfile; candfile="$(mktemp)"; admfile="$(mktemp)"
    trap 'rm -f "$candfile" "$admfile"' RETURN
    local skips=()

    if [ -n "$members_arg" ]; then
        local _mid _row
        for _mid in $(printf '%s' "$members_arg" | tr ',' ' '); do
            [ -n "$_mid" ] || continue
            _row="$(printf '%s\n' "$certs" | awk -v id="$_mid" '$1==id{print $1, $2; exit}')"
            if [ -z "$_row" ]; then
                skips+=("$_mid: not CERTIFIED")
                continue
            fi
            printf '%s\n' "$_row" >> "$candfile"
        done
    else
        local all_ids=() _cid
        while read -r _cid _ _; do [ -n "$_cid" ] && all_ids+=("$_cid"); done <<< "$certs"
        local prio_json="[]"
        [ "${#all_ids[@]}" -gt 0 ] && prio_json="$(bdjson show "${all_ids[@]}" 2>/dev/null)"
        [ -n "$prio_json" ] || prio_json="[]"
        PRIO_JSON="$prio_json" queue_sort_rows "$repo" "$base_sha" \
            < <(printf '%s\n' "$certs") \
            | awk '{print $5, $6}' > "$candfile"
    fi

    # Second line of defence, matching batch.sh's own admission check: a CERTIFIED
    # landstate whose bead is neither closed nor carrying the submitted label is not
    # admissible. An empty bd answer is "unknown", never "admit".
    local _cid2 _ctip2 _st
    while read -r _cid2 _ctip2; do
        [ -n "$_cid2" ] || continue
        _st="$(spira_bead_status "$_cid2")"
        if [ -n "$_st" ] && [ "$_st" != closed ] \
           && ! bead_has_label "$(bdjson show "$_cid2" 2>/dev/null)" "${SPIRA_SUBMITTED_LABEL:-spira-submitted}"; then
            skips+=("$_cid2: bead status=$_st (not closed, not submitted)")
            continue
        fi
        printf '%s %s\n' "$_cid2" "$_ctip2" >> "$admfile"
    done < "$candfile"
    cp "$admfile" "$candfile"

    # Assemble in a scratch worktree from the land ref — never the landing pass's own
    # .batch-<repo> worktree, so a hand-invoked open-batch cannot collide with a live
    # automatic cut.
    local wt; wt="$SPIRA_RUN/worktree/.open-batch-$name-$$"
    git -C "$repo" worktree prune 2>/dev/null || true
    mkdir -p "$(dirname "$wt")"
    git -C "$repo" worktree add -q --detach "$wt" "$base_sha" 2>/dev/null || {
        printf 'queue.sh open-batch: cannot create assembly worktree\n' >&2
        return 1
    }

    local members=() member_ids=() _bid _btip _reason
    while read -r _bid _btip; do
        [ -n "$_bid" ] || continue
        if git -C "$wt" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" \
               merge --no-edit --no-ff -m "spira: land $_bid" "$_btip" >/dev/null 2>&1; then
            members+=("$_bid:$_btip")
            member_ids+=("$_bid")
        else
            git -C "$wt" merge --abort 2>/dev/null || true
            if _base_conflict "$repo" "$base_sha" "$_btip"; then
                _reason="conflicts with base"
            else
                _reason="conflicts with batch"
            fi
            skips+=("$_bid: $_reason")
        fi
    done < "$candfile"

    local _s
    for _s in "${skips[@]:-}"; do
        [ -n "$_s" ] && printf 'queue.sh open-batch: skip — %s\n' "$_s"
    done

    if [ "${#members[@]}" -eq 0 ]; then
        printf 'queue.sh open-batch: no cut — nothing admissible for %s\n' "$name"
        git -C "$repo" worktree remove -f "$wt" 2>/dev/null || true
        return 1
    fi

    format_batch "$wt" "$base_sha" "$name"
    local batch_head; batch_head="$(git -C "$wt" rev-parse HEAD 2>/dev/null)"

    local stamp batch_br
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    batch_br="spira/queue/$stamp"

    if [ "$dry_run" -eq 1 ]; then
        printf 'queue.sh open-batch: dry-run for %s\n' "$name"
        printf 'members:\n'
        for _s in "${members[@]}"; do printf '  %s\n' "$_s"; done
        printf 'merge head: %s\n' "$batch_head"
        printf 'would write open record:\n'
        printf '  pr=<pending>\n'
        printf '  head=%s\n' "$batch_head"
        printf '  base=%s\n' "$base_sha"
        printf '  members=%s\n' "${members[*]}"
        printf '  opened=<pending>\n'
        printf '  branch=%s\n' "$batch_br"
        git -C "$repo" worktree remove -f "$wt" 2>/dev/null || true
        return 0
    fi

    git -C "$repo" branch -f "$batch_br" "$batch_head" 2>/dev/null || true
    git -C "$repo" worktree remove -f "$wt" 2>/dev/null || true

    local lg_out="" lg_rc=0
    if [ "$skip_pregate" -eq 1 ]; then
        printf 'queue.sh open-batch: pre-flight gate skipped (--skip-pregate) — CI is the authority\n'
    else
        _PF_DEADLINE=$(( $(date +%s) + ${SPIRA_PREFLIGHT_WALL_SECS:-240} ))
        lg_out="$(_pf_gate "$batch_br" "$name" "$stamp")"
        lg_rc=$?
        if [ "$lg_rc" -eq 124 ]; then
            printf 'queue.sh open-batch: pre-flight hit its wall — opening the PR, CI decides\n'
            lg_rc=0
        fi
    fi

    if [ "$lg_rc" -ne 0 ]; then
        printf 'queue.sh open-batch: local pre-flight gate failed for %s — not opening a batch\n' "$batch_br" >&2
        printf '%s\n' "$lg_out" >&2
        printf 'queue.sh open-batch: retry with --skip-pregate to let CI be the gate\n' >&2
        git -C "$repo" branch -D "$batch_br" 2>/dev/null || true
        return 1
    fi

    if ! spira_git_push "$repo" -q "${remote:-origin}" "${batch_head}:refs/heads/${batch_br}" 2>/dev/null; then
        printf 'queue.sh open-batch: could not push %s\n' "$batch_br" >&2
        return 1
    fi

    local forge="${SPIRA_FORGE:-$HERE/forge.sh}"
    local member_titles_json
    member_titles_json="$(bdjson show "${member_ids[@]}" 2>/dev/null)" || member_titles_json="[]"

    local pr_body mid t
    pr_body="$(
        printf 'Merge-queue batch: %d beads for %s, onto %s.\n\n' \
            "${#members[@]}" "$name" "$base_branch"
        if [ "$skip_pregate" -eq 1 ]; then
            printf 'Opened with --skip-pregate: the local pre-flight gate was not run for this batch; CI is the gate.\n\n'
        fi
        for mid in "${member_ids[@]}"; do
            t="$(printf '%s\n' "$member_titles_json" | python3 -c "
import json, sys
data = json.load(sys.stdin)
items = data if isinstance(data, list) else [data]
t = next((str(i.get('title','')) for i in items if i.get('id') == '$mid'), '')
print((t[:120] if t else '(title unavailable)') or '(title unavailable)')
" 2>/dev/null)" || t="(title unavailable)"
            printf -- '- %s — %s\n' "$mid" "${t:-(title unavailable)}"
        done
    )"

    local pr_n
    pr_n="$(printf '%s' "$pr_body" \
            | "$forge" pr-create "$repo" "$batch_br" "$base_branch" \
                "queue: ${#members[@]} beads for $name" 2>/dev/null)" || {
        printf 'queue.sh open-batch: forge pr-create failed for %s\n' "$batch_br" >&2
        return 1
    }
    [ -n "${pr_n:-}" ] || {
        printf 'queue.sh open-batch: forge returned no PR number for %s\n' "$batch_br" >&2
        return 1
    }

    # Write the open record and mark members BATCHED only now that the PR exists and its
    # number is known — writing either before the PR was certain was a real defect.
    local bdir; bdir="$(dirname "$(_batch_open_file "$name")")"
    mkdir -p "$bdir"
    local opened_at; opened_at="$(date +%s)"
    {
        printf 'pr=%s\n'      "$pr_n"
        printf 'head=%s\n'    "$batch_head"
        printf 'base=%s\n'    "$base_sha"
        printf 'members=%s\n' "${members[*]}"
        printf 'opened=%s\n'  "$opened_at"
        printf 'branch=%s\n'  "$batch_br"
    } > "$(_batch_open_file "$name")"

    local _mm _mid2 _mtip2
    for _mm in "${members[@]}"; do
        _mid2="${_mm%%:*}"; _mtip2="${_mm##*:}"
        land_mark "$_mid2" BATCHED "$_mtip2"
    done
    rm -f "$SPIRA_RUN/queue-stuck-$name" 2>/dev/null || true

    printf 'QUEUE BATCH %s repo=%s members=%d gate_seconds=0 verdict=green source=open-batch\n' \
        "$opened_at" "$name" "${#members[@]}" >> "$SPIRA_RUN/landing.log" 2>/dev/null || true

    printf 'queue.sh open-batch: PR %s opened — %d branches (%s)\n' \
        "$pr_n" "${#members[@]}" "$batch_br"
}

case "${1:-}" in
    submit)  shift; cmd_submit "$@" ;;
    protect) shift; cmd_protect "$@" ;;
    stats)   cmd_stats ;;
    flush)   shift; cmd_flush "$@" ;;
    step)    shift; cmd_step "$@" ;;
    eject)   shift; cmd_eject "$@" ;;
    abandon) shift; cmd_abandon "$@" ;;
    open-batch) shift; cmd_open_batch "$@" ;;
    *) printf 'usage: queue.sh submit <branch> [<repo>] | queue.sh protect [<repo>] | queue.sh stats | queue.sh flush [<repo>] | queue.sh step <repo> | queue.sh eject <id> [--reason <text>] [--dry-run] [<repo>] | queue.sh abandon [<repo>] --reason <text> [--dry-run] | queue.sh open-batch [<repo>] [--members <ids>] [--skip-pregate] [--dry-run]\n' >&2; exit 2 ;;
esac
