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
#   queue.sh claim [<repo>] --reason <text> [--force]
#   queue.sh release [<repo>]
#   queue.sh land-local [<repo>] --head <sha> --members <id:tip[,id:tip...]>
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
# ONE WRITER PER OPEN BATCH (sp-91hb5). The open record names an owner: normally the
# automatic pipeline (batcher, or the actor that ran open-batch by hand), which every
# automatic path already cooperates with. claim hands exclusive control to the concierge —
# for a hand edit to the round branch — and every mutator here (eject, abandon), verdict.sh
# and the batcher then refuse rather than race it, naming the owner and the override
# (SPIRA_QUEUE_OWNER_OVERRIDE=1). release hands it back. Every mutation the recorded owner
# actually makes mails the concierge mailbox, which wakes the pane in real time
# (law-machine-events-wake-in-real-time) — so the owner's own legitimate work is never a
# silent surprise either.
#
# land-local: the queue.local ending — fast-forwards the repo's local landing ref to a
# round head with no PR, refusing (and writing nothing) unless the head descends from the
# ref's current tip. Archives the head at refs/archive/rounds/<n>, land_marks every member
# LANDED and closes its bead via bead_close_on_land. THE ONLY WRITER of that ref; runs
# under the repo's queue lock, skipped via SPIRA_QUEUE_LOCK_HELD=1 for a caller (the
# batcher) that already holds it, so it cannot deadlock on its own lock.
#
# covers: spira/queue.sh spira/suites.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/lib.sh"
. "$HERE/lc.sh"


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
        base_remote="$(ref_remote "$base" "$repo")" || base_remote=""
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
    printf 'queue.sh protect:   Without them a red batch PR is left for the batcher persona to judge.\n'
}

# _batch_cut <repo-name> — the merge-queue's round cutter. batch.sh's own sweep runs first,
# unconditionally, every pass (reconciliation, the stuck-queue alert, the open batch's DIRTY
# check — none of that is the cut itself, see batch.sh's own header); the batcher crate
# (sp-jzfog) then owns the round: trigger, membership, local proving and the PR (sp-vsob2
# retires batch.sh's own cut, the bisect-forced cut and the local-gate-before-cut in its
# favour, once it had proven itself live on 2026-09-24).
_batch_cut() {
    bash "$HERE/batch.sh" "$1"
    if [ -z "${SPIRA_BATCHER_BIN:-}" ] || [ ! -x "$SPIRA_BATCHER_BIN" ]; then
        printf 'queue.sh: SPIRA_BATCHER_BIN not available — cannot cut a round for %s\n' "$1" >&2
        return 1
    fi
    "$SPIRA_BATCHER_BIN" cut "$1"
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

# spira-lc's own OPEN-batch lifecycle (sp-o7nbr.5, same shape as batch.sh's own
# _lc_cut_batch/_abandon_open_batch, sp-o7nbr.2): every member needs a bead row before
# `cut` will look at it — create-bead-if-absent, never a shortcut to CERTIFIED (that
# transition belongs to the CERTIFIED-producing call sites, siblings' turf) — then cut.
# A refusal (a member not CERTIFIED at this exact tip on spira-lc — expected while those
# call sites are still being cut over) is not unwound: the caller's own open-batch record
# simply carries no batch_id/version afterward, so a later abandon/eject on this record
# skips the new-system call, same as a pre-cutover record does.
_lc_cut_batch() {   # _lc_cut_batch <batch-id> <repo> <head> <base> <actor> <id:tip> [<id:tip>...]
    local batch_id="$1" repo="$2" head="$3" base="$4" actor="$5"; shift 5
    local _m members_csv=""
    for _m in "$@"; do
        lcq create-bead "${_m%%:*}" >/dev/null 2>&1 || true
        members_csv="${members_csv:+$members_csv,}$_m"
    done
    lcq cut "$batch_id" --repo "$repo" --head "$head" --base "$base" --members "$members_csv" --actor "$actor"
}

# _lc_batch_state_version <batch-id> -> "STATE VERSION" on spira-lc, or nothing (rc=1) when
# the batch row does not exist there (a pre-cutover batch, or a cut that never applied).
# Queried fresh rather than cached: the state a caller's own record last saw goes stale the
# moment CI or another operator action moves it.
_lc_batch_state_version() {
    local out; out="$(lcq show-batch "$1" 2>/dev/null)" || return 1
    printf '%s' "$out" | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(1)
state, version = d.get('state'), d.get('version')
if state is None or version is None:
    sys.exit(1)
print(state, version)
" 2>/dev/null
}

# _lc_abandon_batch <batch-id> <actor> <reason> — abandon-batch accepts from any
# non-terminal state, so the only thing fetched fresh is what CAS needs: state and version.
_lc_abandon_batch() {
    local batch_id="$1" actor="$2" reason="$3" sv state version
    sv="$(_lc_batch_state_version "$batch_id")" || return 1
    read -r state version <<< "$sv"
    [ -n "$state" ] || return 1
    lcq abandon-batch "$batch_id" --expect "$state" --version "$version" --actor "$actor" --reason "$reason"
}

# _lc_eject_member <batch-id> <bead-id> <actor> <reason> — legal only from OPEN/CI_RUNNING;
# ejecting once the batch has moved past CI (GREEN/ATTRIBUTING/REBUILDING) is refused like
# any other illegal transition, same as calling this after the batch already landed/settled.
_lc_eject_member() {
    local batch_id="$1" bead_id="$2" actor="$3" reason="$4" sv state version
    sv="$(_lc_batch_state_version "$batch_id")" || return 1
    read -r state version <<< "$sv"
    [ -n "$state" ] || return 1
    lcq eject-member "$batch_id" --bead-id "$bead_id" --expect "$state" --version "$version" --actor "$actor" --reason "$reason"
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

    local actor; actor="${SPIRA_QUEUE_ACTOR:-${BEADS_ACTOR:-${SPIRA_AEON:+aeon-$SPIRA_AEON}}}"
    actor="${actor:-${USER:-unknown}}"

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
    local members_val="" pr_n="" tip="" found=0 new_members="" lc_batch_id=""
    if [ -f "$open_file" ]; then
        members_val="$(grep '^members=' "$open_file" | head -1)"
        members_val="${members_val#members=}"
        pr_n="$(grep '^pr=' "$open_file" | head -1)"; pr_n="${pr_n#pr=}"
        lc_batch_id="$(grep '^batch_id=' "$open_file" | head -1)"; lc_batch_id="${lc_batch_id#batch_id=}"

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

    if [ "$found" -eq 1 ] && queue_owner_refused "$(queue_batch_owner "$open_file")" "$actor" "queue.sh eject"; then
        return 1
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
        printf 'dry-run: would return bead %s to spira-lc via a Returned event\n' "$id"
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

    # THE LIFECYCLE EVENT, NOT bead_reopen/bd label (sp-rlyl0): pulling one member out of an
    # open batch mid-CI is the delivery-exit event legal from IN_DELIVERY — Returned, the
    # same one verdict.sh's own settle cascade applies to a red batch's ejected member.
    # Best-effort like every lc.sh caller: batcher-cut (sp-vsob2) does not write batch_id/
    # version into the open-batch record yet, so this is CANNOT_TELL in production today.
    lc_returned "$id" "${reason:-ejected}" >/dev/null 2>&1 || true
    # The assignee clear is bd metadata this tool still keeps (not a status/label write) —
    # a dead actor's name should not survive an eject any more than a reopen.
    release_claim "$id"

    # spira-lc's own manual eject (sp-o7nbr.5): batch_id is present only on a record this
    # session's own open-batch wrote and cut applied there — a pre-cutover record, or one
    # whose cut refused, has none, and the call is skipped rather than CASing against a
    # batch spira-lc never wrote. Legal only while OPEN/CI_RUNNING, same as the shell path
    # above it mirrors; a batch already past CI refuses here too.
    if [ -n "$lc_batch_id" ]; then
        local _lc_out _lc_rc
        _lc_out="$(_lc_eject_member "$lc_batch_id" "$id" "queue.sh" "${reason:-ejected}")"
        _lc_rc=$?
        if [ "$_lc_rc" -eq 0 ]; then
            printf 'queue.sh eject: %s ejected on spira-lc (returned to CERTIFIED there)\n' "$id"
        else
            printf 'queue.sh eject: spira-lc eject-member refused for %s (rc=%d): %s\n' "$id" "$_lc_rc" "$_lc_out" >&2
        fi
    fi

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

    queue_notify_concierge "$name" "$id ejected (queue.sh eject)" \
        "$id ejected from PR $pr_n by $actor.${reason:+ Reason: $reason}"$'\n'"Survivors returned to CERTIFIED: ${new_members:-<none>}"

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

    local actor; actor="${SPIRA_QUEUE_ACTOR:-${BEADS_ACTOR:-${SPIRA_AEON:+aeon-$SPIRA_AEON}}}"
    actor="${actor:-${USER:-unknown}}"

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

    queue_owner_refused "$(queue_batch_owner "$open_file")" "$actor" "queue.sh abandon" && return 1

    local pr_n members_val branch_val lc_batch_id
    pr_n="$(grep '^pr=' "$open_file" | head -1)"; pr_n="${pr_n#pr=}"
    members_val="$(grep '^members=' "$open_file" | head -1)"; members_val="${members_val#members=}"
    branch_val="$(grep '^branch=' "$open_file" | head -1)"; branch_val="${branch_val#branch=}"
    lc_batch_id="$(grep '^batch_id=' "$open_file" | head -1)"; lc_batch_id="${lc_batch_id#batch_id=}"

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

    # spira-lc's own whole-batch abandon (sp-o7nbr.5, same guard shape as cmd_eject's
    # eject-member call): batch_id is present only on a record whose cut applied there.
    # abandon-batch accepts from any non-terminal state, one cascade returning every
    # member to CERTIFIED/SUBMITTED — the same outcome the LANDSTATE loop below produces
    # by hand, on the machine that is cutting over to replace it.
    if [ -n "$lc_batch_id" ]; then
        local _lc_out _lc_rc
        _lc_out="$(_lc_abandon_batch "$lc_batch_id" "queue.sh" "$reason")"
        _lc_rc=$?
        if [ "$_lc_rc" -eq 0 ]; then
            printf 'queue.sh abandon: %s abandoned on spira-lc\n' "$lc_batch_id"
        else
            printf 'queue.sh abandon: spira-lc abandon-batch refused for %s (rc=%d): %s\n' "$lc_batch_id" "$_lc_rc" "$_lc_out" >&2
        fi
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

    queue_notify_concierge "$name" "batch abandoned (queue.sh abandon)" \
        "PR $pr_n abandoned by $actor. Reason: $reason_clean"$'\n'"Members: ${member_audit:-<none>}"

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
    local actor; actor="${SPIRA_QUEUE_ACTOR:-operator}"

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
        local _ob_owner; _ob_owner="$(queue_batch_owner "$(_batch_open_file "$name")")"
        printf 'queue.sh open-batch: a batch is already open for %s (owner=%s) — no override; abandon it first (queue.sh abandon)\n' \
            "$name" "${_ob_owner:-batcher}" >&2
        return 1
    fi

    local base base_sha remote base_branch
    base="$(spira_landref "$repo")" || {
        printf 'queue.sh open-batch: cannot resolve landing ref for %s\n' "$name" >&2; return 1
    }
    base_sha="$(git -C "$repo" rev-parse "$base" 2>/dev/null)" || {
        printf 'queue.sh open-batch: cannot resolve %s\n' "$base" >&2; return 1
    }
    remote="$(ref_remote "$base" "$repo")"
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
               merge --no-edit --no-ff -m "$(land_subject "$_bid")" "$_btip" >/dev/null 2>&1; then
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
        printf '  owner=%s\n' "$actor"
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
        printf 'owner=%s\n'   "$actor"
    } > "$(_batch_open_file "$name")"

    # spira-lc's own OPEN-batch lifecycle (sp-o7nbr.5): one cut cascade creates the batch
    # row and every batch_member row there, keyed by this same stamp so the batch_id
    # correlates with the PR branch. Refusing (a member not yet CERTIFIED at this tip on
    # spira-lc — expected while the CERTIFIED-producing call sites are still being cut
    # over) does not unwind the PR already opened above; it just leaves batch_id/version
    # unset on this open record, so a later abandon/eject on it skips the new-system call,
    # same as a pre-cutover record does.
    local _lc_batch_id="${name}-${stamp}"
    local _lc_out _lc_rc
    _lc_out="$(_lc_cut_batch "$_lc_batch_id" "$name" "$batch_head" "$base_sha" "queue.sh" "${members[@]}")"
    _lc_rc=$?
    if [ "$_lc_rc" -eq 0 ]; then
        local _lc_version
        _lc_version="$(printf '%s' "$_lc_out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("version",""))' 2>/dev/null)"
        if [ -n "$_lc_version" ]; then
            {
                printf 'batch_id=%s\n' "$_lc_batch_id"
                printf 'version=%s\n'  "$_lc_version"
            } >> "$(_batch_open_file "$name")"
            printf 'queue.sh open-batch: %s cut on spira-lc (version=%s)\n' "$_lc_batch_id" "$_lc_version"
        fi
    else
        printf 'queue.sh open-batch: spira-lc cut refused for %s (rc=%d): %s\n' "$_lc_batch_id" "$_lc_rc" "$_lc_out" >&2
    fi

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

# cmd_claim: the Concierge's own tool for taking exclusive hand control of an already-open
# batch before editing its round branch directly — the gap sp-91hb5 closes. Every other
# mutator (verdict.sh, the batcher, queue.sh eject/abandon) checks queue_owner_refused and
# backs off while owner=concierge. Refuses if already claimed; --force takes it anyway
# (the claim is a courtesy against concurrent hand edits, not a lock against the operator).
cmd_claim() {
    local name="" reason="" force=0
    while [ $# -gt 0 ]; do
        case "$1" in
        --reason)   shift; reason="${1:-}"; shift ;;
        --reason=*) reason="${1#--reason=}"; shift ;;
        --force)    force=1; shift ;;
        -*)         printf 'queue.sh claim: unknown option: %s\n' "$1" >&2; return 2 ;;
        *)          name="$1"; shift ;;
        esac
    done
    [ -n "$reason" ] || {
        printf 'queue.sh claim: --reason is required — say what the hand edit is for\n' >&2
        return 2
    }
    [ -n "$name" ] || name="$(spira_home_repo)"
    repo_root "$name" >/dev/null 2>&1 || {
        printf 'queue.sh claim: no such repo: %s\n' "$name" >&2; return 1
    }

    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    { exec 9>"$lockfile"; } 2>/dev/null \
        || { printf 'queue.sh claim: cannot open lock file for %s\n' "$name" >&2; return 1; }
    if ! flock -n 9; then
        printf 'queue.sh claim: another queue operation holds the lock for %s\n' "$name" >&2
        return 1
    fi

    local open_file="${SPIRA_QUEUE_DIR:?}/$name/open"
    [ -f "$open_file" ] || {
        printf 'queue.sh claim: no open batch for %s\n' "$name" >&2; return 1
    }

    local cur; cur="$(queue_batch_owner "$open_file")"
    if [ "$cur" = concierge ] && [ "$force" -eq 0 ]; then
        printf 'queue.sh claim: %s is already claimed by concierge — pass --force to reclaim\n' "$name" >&2
        return 1
    fi

    { grep -vE '^(owner|pre_claim_owner|claim_reason)=' "$open_file" 2>/dev/null
      printf 'owner=concierge\n'
      printf 'pre_claim_owner=%s\n' "$cur"
      printf 'claim_reason=%s\n' "${reason//$'\n'/ }"
    } > "$open_file.$$" && mv -f "$open_file.$$" "$open_file"

    queue_notify_concierge "$name" "batch claimed for hand-edit" \
        "Claimed the open batch for $name (was: ${cur:-<none>}). Reason: $reason"

    printf 'queue.sh claim: %s claimed for concierge (was: %s)\n' "$name" "${cur:-<none>}"
}

# cmd_release: hands the open batch back to whichever owner held it before the claim
# (batcher, or the legacy default) so the automatic pipeline resumes settling it.
cmd_release() {
    local name=""
    while [ $# -gt 0 ]; do
        case "$1" in
        -*) printf 'queue.sh release: unknown option: %s\n' "$1" >&2; return 2 ;;
        *)  name="$1"; shift ;;
        esac
    done
    [ -n "$name" ] || name="$(spira_home_repo)"
    repo_root "$name" >/dev/null 2>&1 || {
        printf 'queue.sh release: no such repo: %s\n' "$name" >&2; return 1
    }

    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    { exec 9>"$lockfile"; } 2>/dev/null \
        || { printf 'queue.sh release: cannot open lock file for %s\n' "$name" >&2; return 1; }
    if ! flock -n 9; then
        printf 'queue.sh release: another queue operation holds the lock for %s\n' "$name" >&2
        return 1
    fi

    local open_file="${SPIRA_QUEUE_DIR:?}/$name/open"
    [ -f "$open_file" ] || {
        printf 'queue.sh release: no open batch for %s\n' "$name" >&2; return 1
    }
    if [ "$(queue_batch_owner "$open_file")" != concierge ]; then
        printf 'queue.sh release: %s is not claimed by concierge\n' "$name" >&2
        return 1
    fi

    local restore; restore="$(grep '^pre_claim_owner=' "$open_file" 2>/dev/null | tail -1 | cut -d= -f2-)"
    { grep -vE '^(owner|pre_claim_owner|claim_reason)=' "$open_file" 2>/dev/null
      printf 'owner=%s\n' "$restore"
    } > "$open_file.$$" && mv -f "$open_file.$$" "$open_file"

    printf 'queue.sh release: %s released back to %s\n' "$name" "${restore:-<none>}"
}

# cmd_land_local: see the header comment above (land-local:).
cmd_land_local() {
    local name="" head="" members_arg=""
    if [ "${SPIRA_FAYTH:-}" = czar ] && [ -n "${SPIRA_CZAR_CLASS:-}" ]; then
        bash "$HERE/czar-fence.sh" "$SPIRA_CZAR_CLASS" || return 1
    fi

    while [ $# -gt 0 ]; do
        case "$1" in
        --head)      shift; head="${1:-}"; shift ;;
        --head=*)    head="${1#--head=}"; shift ;;
        --members)   shift; members_arg="${1:-}"; shift ;;
        --members=*) members_arg="${1#--members=}"; shift ;;
        -*)          printf 'queue.sh land-local: unknown option: %s\n' "$1" >&2; return 2 ;;
        *)           name="$1"; shift ;;
        esac
    done
    [ -n "$name" ] || name="$(spira_home_repo)"
    [ -n "$head" ] || {
        printf 'queue.sh land-local: --head is required\n' >&2; return 2
    }
    [ -n "$members_arg" ] || {
        printf 'queue.sh land-local: --members is required\n' >&2; return 2
    }

    local repo; repo="$(repo_root "$name" 2>/dev/null)" || {
        printf 'queue.sh land-local: no such repo: %s\n' "$name" >&2; return 1
    }
    local mode; mode="$(repo_land "$name")"
    [ "$mode" = "queue.local" ] || {
        printf 'queue.sh land-local: repo is not in queue.local mode (mode=%s)\n' "$mode" >&2; return 1
    }

    local base; base="$(spira_landref "$repo")" || {
        printf 'queue.sh land-local: cannot resolve landing ref for %s\n' "$name" >&2; return 1
    }
    # queue.local's base is a LOCAL branch (e.g. local/main) that happens to contain a
    # slash, never remote-tracking — so unlike every queue.forge caller, the branch name is
    # $base UNSPLIT: ref_branch's ${ref#*/} strip assumes a remote/branch shape and would
    # mangle "local/main" into "main", a branch this land is not the one to move. A row
    # whose declared base IS a real remote ref is misconfigured for this mode.
    if ref_remote "$base" "$repo" >/dev/null 2>&1; then
        printf 'queue.sh land-local: %s resolves to a remote-tracking ref (%s) — not a queue.local base\n' \
            "$name" "$base" >&2
        return 1
    fi
    local base_branch="$base"

    head="$(git -C "$repo" rev-parse --verify -q "$head" 2>/dev/null)" || {
        printf 'queue.sh land-local: cannot resolve head\n' >&2; return 1
    }

    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    if [ "${SPIRA_QUEUE_LOCK_HELD:-0}" != 1 ]; then
        { exec 9>"$lockfile"; } 2>/dev/null \
            || { printf 'queue.sh land-local: cannot open lock file for %s\n' "$name" >&2; return 1; }
        if ! flock -n 9; then
            printf 'queue.sh land-local: another queue operation holds the lock for %s\n' "$name" >&2
            return 1
        fi
    fi

    local base_sha; base_sha="$(git -C "$repo" rev-parse --verify -q "$base_branch" 2>/dev/null)" || {
        printf 'queue.sh land-local: cannot resolve %s\n' "$base_branch" >&2; return 1
    }

    if ! git -C "$repo" merge-base --is-ancestor "$base_sha" "$head" 2>/dev/null; then
        printf 'queue.sh land-local: %s does not fast-forward from %s (%s) — refused, nothing changed\n' \
            "$head" "$base" "$base_sha" >&2
        return 1
    fi

    # A CAS, not a plain write: refuses instead of clobbering if something else moved the
    # ref between the resolve above and here (this is exactly the "another writer" case,
    # not just belt-and-suspenders around the lock).
    if ! git -C "$repo" update-ref "refs/heads/$base_branch" "$head" "$base_sha" 2>/dev/null; then
        printf 'queue.sh land-local: %s moved concurrently — refused, nothing changed\n' "$base" >&2
        return 1
    fi

    local seqfile="${SPIRA_QUEUE_DIR:?}/$name/round-seq" n
    n="$(cat "$seqfile" 2>/dev/null)"; case "$n" in ''|*[!0-9]*) n=0 ;; esac
    n=$(( n + 1 ))
    local archive_ref="refs/archive/rounds/$n"
    git -C "$repo" update-ref "$archive_ref" "$head" 2>/dev/null || true
    printf '%s\n' "$n" > "$seqfile.$$" 2>/dev/null && mv -f "$seqfile.$$" "$seqfile" 2>/dev/null

    local _m _mid _mtip
    for _m in $(printf '%s' "$members_arg" | tr ',' ' '); do
        [ -n "$_m" ] || continue
        _mid="${_m%%:*}"; _mtip="${_m#*:}"
        [ "$_mtip" = "$_m" ] && _mtip="$head"
        land_mark "$_mid" LANDED "$_mtip"
        gh_issue_closeout "$_mid" "$head" "$repo" || true
        bead_close_on_land "$_mid" "$head" || true
        printf 'queue.sh land-local: %s landed at %s\n' "$_mid" "$head"
    done

    queue_notify_concierge "$name" "local landing (round $n)" \
        "$base fast-forwarded to $head (round $n, archived at $archive_ref). Members: $members_arg"

    printf 'queue.sh land-local: %s fast-forwarded to %s (round %d, archived at %s)\n' \
        "$base" "$head" "$n" "$archive_ref"
}

main() {
    case "${1:-}" in
        submit)  shift; cmd_submit "$@" ;;
        protect) shift; cmd_protect "$@" ;;
        stats)   cmd_stats ;;
        flush)   shift; cmd_flush "$@" ;;
        step)    shift; cmd_step "$@" ;;
        eject)   shift; cmd_eject "$@" ;;
        abandon) shift; cmd_abandon "$@" ;;
        open-batch) shift; cmd_open_batch "$@" ;;
        claim)   shift; cmd_claim "$@" ;;
        release) shift; cmd_release "$@" ;;
        land-local) shift; cmd_land_local "$@" ;;
        *) printf 'usage: queue.sh submit <branch> [<repo>] | queue.sh protect [<repo>] | queue.sh stats | queue.sh flush [<repo>] | queue.sh step <repo> | queue.sh eject <id> [--reason <text>] [--dry-run] [<repo>] | queue.sh abandon [<repo>] --reason <text> [--dry-run] | queue.sh open-batch [<repo>] [--members <ids>] [--skip-pregate] [--dry-run] | queue.sh claim [<repo>] --reason <text> [--force] | queue.sh release [<repo>] | queue.sh land-local [<repo>] --head <sha> --members <id:tip[,id:tip...]>\n' >&2; return 2 ;;
    esac
}

# Sourced (a suite wants _lc_cut_batch/_lc_abandon_batch/_lc_eject_member without a real
# dispatch) vs executed: matches batch.sh's own guard, and for the same reason.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    main "$@"
    exit $?
fi
