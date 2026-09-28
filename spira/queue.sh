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
#   queue.sh publish [<repo>]
#   queue.sh to-forge [<repo>]
#   queue.sh to-local [<repo>]
#   queue.sh rollback-local [<repo>]
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
# ref's current tip. Production runs built artifacts, not a checkout, so a land also
# packages the round head's source with the binaries its own tree's --with-bins corpus
# built (build-tarball.sh --bin-dir) and activates the result atomically (activate.sh) —
# refusing, ref move reverted, if either step fails or the corpus is missing. Archives the
# head at refs/archive/rounds/<n>, land_marks every member LANDED and closes its bead via
# bead_close_on_land. THE ONLY WRITER of that ref; runs under the repo's queue lock,
# skipped via SPIRA_QUEUE_LOCK_HELD=1 for a caller (the batcher) that already holds it, so
# it cannot deadlock on its own lock. Also runs row 4's
# divergence check (queue_local_check_divergence) against the CACHED forge remote-tracking
# ref before the round build — no fetch, so it never puts the forge on this round's critical
# path — alarming, never refusing: only publish stops on a divergence, a round build does not.
#
# rollback-local: re-activates the previous round's retained release and resets the ref back
# to its archived head. Bead state is untouched — a round's beads stay LANDED regardless of
# what is currently running.
#
# publish: the queue.local publish queue (row 3 of the local/main design). Pushes
# local/main's commits since the forge target's own current tip to a new forge branch
# (spira/publish/<stamp>) and opens ONE PR into it — the local round already fast-forwarded
# sequentially, so this pushes the range as-is: no assembled merge commit, identical SHAs.
# Refuses, writing nothing, if the forge target is not an ancestor of local/main (something
# else moved it, caught by row 4's queue_local_check_divergence — see lib.sh, which alarms
# the concierge once per foreign tip naming the foreign commits) or if there is nothing new
# to publish. The record lives at
# queue/<repo>/publish, deliberately NOT queue/<repo>/open: `open` is the queue.forge batch
# builder's own file, and a publish record there would make batch_open/merge_one treat a
# publish-in-flight as a batch-in-flight, blocking every local cut behind a PR the local
# queue never opened. verdict.sh settles it separately (green fast-forwards the forge
# target with no land_mark and no bead close; red runs local attribution and files one
# fix-forward bead) — see verdict.sh's own header. Skips its own lock under
# SPIRA_QUEUE_LOCK_HELD=1, same as land-local, for to-forge below.
#
# to-forge/to-local: row 6 of the local/main design — the transition between queue.local
# and queue.forge. to-forge holds the repo's queue lock for the whole move (so no local
# round can land underneath it), runs a final publish, blocks until the forge PR settles
# (refusing, unchanged, on red or a timeout), verifies the forge target and local/main are
# now identical, then flips repo-map's (and, if a spira.toml is in force, its matching
# [repo.*]) land/base columns and archives local/main at refs/archive/<base>. to-local is
# the reverse: it re-derives a local branch from the forge base (reusing an old archive
# when one exists, refusing if it does not fast-forward from there) before flipping back.
# Both refuse, writing nothing, if repo-map and spira.toml already disagree about the row.
#
# covers: spira/queue.sh spira/verdict.sh spira/suites.sh spira/conf.sh
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

    case "$mode" in
    queue|queue.local)
        case "$br" in
        spira/*|spira-suite-state/*) ;;
        *)
            printf 'queue.sh submit: %s: queue mode requires a branch under spira/ or spira-suite-state/\n' "$br" >&2
            return 1
            ;;
        esac
        ;;
    esac

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
    queue|queue.local)
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
    # queue.local has no batch PR to cut — _batch_cut's local round (batcher -> land-local)
    # already advanced local/main above, if anything was CERTIFIED — but nothing yet asks
    # the forge to catch up with it. verdict.sh (just above) only SETTLES an already-open
    # publish record; opening the next one, when local/main has pulled ahead again, is this.
    if [ "$(repo_land "$name")" = "queue.local" ]; then
        cmd_publish "$name" 2>&1
    fi
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

# _land_local_bins_dir <repo> <head> -> the --with-bins corpus directory for this tree, if
# it holds at least one executable; prints nothing and fails otherwise. Keyed by TREE, not
# commit, matching testenv-batch.sh's --with-bins cache key exactly, so a head that is its
# own round's tip always finds the corpus that round's own gate run built.
_land_local_bins_dir() {
    local repo="$1" head="$2" tree dir
    tree="$(git -C "$repo" rev-parse -q --verify "${head}^{tree}" 2>/dev/null)" || return 1
    dir="${SPIRA_BATCH_BINS_TARGET_DIR:-${SPIRA_RUN:?}/cargo-target-bins}/$tree/release"
    [ -d "$dir" ] || return 1
    find "$dir" -maxdepth 1 -type f -executable -print -quit 2>/dev/null | grep -q . || return 1
    printf '%s' "$dir"
}

# _land_local_release <repo> <name> <head> <bins_dir> -> package the round head's source
# with the binaries its own tree built and activate the result atomically. Prints the
# release name on success. Tarballs are retained under SPIRA_RELEASES/.tarballs so
# cmd_rollback_local can re-activate one without rebuilding (activate.sh only reads the
# tarball's name when the release directory it names is already unpacked).
_land_local_release() {
    local repo="$1" name="$2" head="$3" bins_dir="$4"
    local retain="${SPIRA_RELEASES:?}/.tarballs"
    mkdir -p "$retain" 2>/dev/null || {
        printf 'queue.sh land-local: cannot create %s\n' "$retain" >&2; return 1; }

    local built
    built="$(bash "$HERE/build-tarball.sh" build --bin-dir "$bins_dir" \
        --repo-name "$name" --name "spira-$head" --output "$retain" "$head" "$repo")"
    if [ -z "$built" ] || [ ! -f "$built" ]; then
        printf 'queue.sh land-local: build-tarball.sh did not produce a tarball\n' >&2
        return 1
    fi

    local act_out act_rc
    act_out="$(bash "$HERE/activate.sh" "$built" 2>&1)"; act_rc=$?
    printf '%s\n' "$act_out" >&2
    [ "$act_rc" -eq 0 ] || return 1
    printf 'spira-%s' "$head"
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

    # Row 4 of the local/main design: check the forge for a foreign divergence before this
    # round build, same as cmd_publish does before a publish. Reads the CACHED remote-
    # tracking ref only — no fetch, so this never puts a forge round trip on the round-build
    # critical path (the intent this whole mode exists for) — and never refuses the build:
    # only cmd_publish's own check stops publishing, this one just alarms early.
    local _dvg_remote _dvg_branch _dvg_sha
    read -r _dvg_remote _dvg_branch < <(spira_publish_forge "$name" 2>/dev/null)
    if [ -n "${_dvg_remote:-}" ] && [ -n "${_dvg_branch:-}" ]; then
        _dvg_sha="$(git -C "$repo" rev-parse -q --verify "refs/remotes/$_dvg_remote/$_dvg_branch" 2>/dev/null)" || _dvg_sha=""
        [ -n "$_dvg_sha" ] && queue_local_check_divergence "$name" "$repo" "$_dvg_sha" "$base_sha" >/dev/null 2>&1
    fi

    if ! git -C "$repo" merge-base --is-ancestor "$base_sha" "$head" 2>/dev/null; then
        printf 'queue.sh land-local: %s does not fast-forward from %s (%s) — refused, nothing changed\n' \
            "$head" "$base" "$base_sha" >&2
        return 1
    fi

    # PRODUCTION RUNS BUILT ARTIFACTS, SO A LAND WITH NONE TO PACKAGE IS REFUSED HERE, before
    # the ref moves — resetting the checkout advances the source but not whatever binary is
    # running against it, and the two can silently disagree about schema or protocol.
    # testenv-batch.sh --with-bins builds the round head's own workspace into this exact
    # directory, keyed by tree so a fast-forward always finds its own corpus, never a
    # stranger's from a different tree that happens to share the same target/release/.
    local bins_dir; bins_dir="$(_land_local_bins_dir "$repo" "$head")" || {
        printf 'queue.sh land-local: no built binaries for %s at %s — run testenv-batch.sh --with-bins first; refused, nothing changed\n' \
            "$head" "${SPIRA_BATCH_BINS_TARGET_DIR:-${SPIRA_RUN:-<SPIRA_RUN unset>}/cargo-target-bins}/<tree>/release" >&2
        return 1
    }

    # A CAS, not a plain write: refuses instead of clobbering if something else moved the
    # ref between the resolve above and here (this is exactly the "another writer" case,
    # not just belt-and-suspenders around the lock).
    if ! git -C "$repo" update-ref "refs/heads/$base_branch" "$head" "$base_sha" 2>/dev/null; then
        printf 'queue.sh land-local: %s moved concurrently — refused, nothing changed\n' "$base" >&2
        return 1
    fi

    # PACKAGE AND ACTIVATE BEFORE ANYTHING ELSE OBSERVES THE LAND. A packaging or activation
    # failure reverts the ref move so this call leaves exactly nothing changed, the same
    # contract every other refusal above already gives its caller.
    local release_name
    if ! release_name="$(_land_local_release "$repo" "$name" "$head" "$bins_dir")"; then
        git -C "$repo" update-ref "refs/heads/$base_branch" "$base_sha" "$head" 2>/dev/null || true
        printf 'queue.sh land-local: packaging/activation failed for %s — reverted, nothing changed\n' "$head" >&2
        return 1
    fi
    printf 'queue.sh land-local: activated %s\n' "$release_name"

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

_publish_file() { printf '%s/%s/publish' "${SPIRA_QUEUE_DIR:?}" "$1"; }
_publish_field() { grep "^$1=" "$2" 2>/dev/null | cut -d= -f2-; }

# _land_mode_toml_mode/_land_mode_toml_base <name> <toml> -> the [repo.<name>] value spira-
# config reads out of a GIVEN toml file, with queue.forge normalized to queue the same way
# repo_land aliases it — spira.toml's own LandMode enum does not alias the two, so a row
# written as one and read as the other would otherwise look like a disagreement that is not
# one. Empty (not an error) when the file has no such repo or key at all.
_land_mode_toml_mode() {
    local name="$1" toml="$2" v
    v="$("$SPIRA_CONFIG_BIN" get "repo.$name.mode" "$toml" 2>/dev/null)"
    [ "$v" = "queue.forge" ] && v=queue
    printf '%s' "$v"
}
_land_mode_toml_base() { "${SPIRA_CONFIG_BIN:?}" get "repo.${1:?}.base" "${2:?}" 2>/dev/null; }

# _land_mode_agrees <name> -> 0 when repo-map and spira.toml already agree about this
# repo's land/base (or there is no spira.toml in force to disagree with, or no
# SPIRA_CONFIG_BIN to ask), 1 and a named diff otherwise. Checked BEFORE either transition
# below touches anything: the two files are already a second source of truth for the same
# fact (see this bead's notes), and a transition is the last place that should paper over an
# existing split rather than refuse on it.
_land_mode_agrees() {
    local name="$1" toml; toml="$(spira_toml_resolve 2>/dev/null)"
    [ -n "$toml" ] || return 0
    [ -x "${SPIRA_CONFIG_BIN:-}" ] || return 0
    local rm_land rm_base tm_land tm_base
    rm_land="$(repo_land "$name")"
    rm_base="$(repo_field "$name" base)"
    tm_land="$(_land_mode_toml_mode "$name" "$toml")"
    tm_base="$(_land_mode_toml_base "$name" "$toml")"
    if [ "$rm_land" != "$tm_land" ] || [ "$rm_base" != "$tm_base" ]; then
        printf 'queue.sh: repo-map and spira.toml already disagree about %s (repo-map: %s|%s; spira.toml: %s|%s) — refused, reconcile by hand first\n' \
            "$name" "$rm_land" "$rm_base" "$tm_land" "$tm_base" >&2
        return 1
    fi
    return 0
}

# _land_mode_write_row <name> <land> <base> — the one writer of a repo's land/base pair,
# across both surfaces that carry it: repo-map (rewrites only columns 3/4 of the matching
# row via NF, same rule repo_field itself reads by — never a fixed position — so path,
# format, gate and lanes survive untouched whatever width the row is) and, when a spira.toml
# is in force, its [repo.<name>] mode/base. A row narrower than the base column (NF<6)
# predates queue.local entirely and is refused rather than guessed at.
_land_mode_write_row() {
    local name="$1" land="$2" base="$3"
    [ -f "$SPIRA_REPO_MAP" ] || {
        printf 'queue.sh: no repo-map at %s\n' "${SPIRA_REPO_MAP:-<unset>}" >&2; return 1
    }
    local tmp; tmp="$(mktemp)"
    if ! awk -v want="$name" -v newland=" $land " -v newbase=" $base " '
        BEGIN { FS = OFS = "|" }
        /^[ \t]*#/ { print; next }
        {
            n = $1; gsub(/^[ \t]+|[ \t]+$/, "", n)
            if (n == want) {
                if (NF < 6) { print "SHORT_ROW" > "/dev/stderr"; exit 1 }
                $3 = newland; $4 = newbase
            }
            print
        }' "$SPIRA_REPO_MAP" > "$tmp" 2>"$tmp.err"; then
        grep -q SHORT_ROW "$tmp.err" 2>/dev/null && \
            printf 'queue.sh: %s row in %s has no base column (NF<6) — add one by hand first\n' \
                "$name" "$SPIRA_REPO_MAP" >&2
        rm -f "$tmp" "$tmp.err"
        return 1
    fi
    rm -f "$tmp.err"
    [ -s "$tmp" ] || { rm -f "$tmp"; printf 'queue.sh: repo-map rewrite produced an empty file — refused\n' >&2; return 1; }
    mv -f "$tmp" "$SPIRA_REPO_MAP" || { rm -f "$tmp"; return 1; }

    local toml; toml="$(spira_toml_resolve 2>/dev/null)"
    if [ -n "$toml" ] && [ -x "${SPIRA_CONFIG_BIN:-}" ]; then
        "$SPIRA_CONFIG_BIN" set "repo.$name.mode" "$land" "$toml" || {
            printf 'queue.sh: repo-map now says %s|%s for %s but spira.toml set mode failed — the two disagree, fix by hand\n' \
                "$land" "$base" "$name" >&2
            return 1
        }
        "$SPIRA_CONFIG_BIN" set "repo.$name.base" "$base" "$toml" || {
            printf 'queue.sh: repo-map and spira.toml mode now say %s for %s but spira.toml set base failed — the two disagree, fix by hand\n' \
                "$land" "$name" >&2
            return 1
        }
    fi
    return 0
}

# _publish_members <repo> <forge-sha> <head-sha> -> "id:tip id:tip ..." — every LANDED
# bead in LANDSTATE whose landed tip is in the range (forge-sha, head-sha]. Reuses
# land-local's own bookkeeping rather than a second record of "what a round contained":
# an id whose tip does not exist in <repo> at all (another repository's bead; LANDSTATE is
# one flat store, not per-repo) fails cat-file and is skipped, same as one already published
# (its tip is an ancestor of forge-sha) or not yet landed (not an ancestor of head-sha).
_publish_members() {
    local repo="$1" forge_sha="$2" head_sha="$3" f id state tip out=""
    [ -d "$LANDSTATE" ] || return 0
    for f in "$LANDSTATE"/*; do
        [ -f "$f" ] || continue
        case "$f" in *.ejected|*.ejected.*|*.rc) continue ;; esac
        id="$(basename "$f")"
        # land_mark writes no trailing newline, so `read ... < "$f"` returns non-zero
        # (EOF) even when it parsed the line fine — a here-string does not have that
        # problem, since `<<<` always appends the newline `read` wants.
        read -r state tip _ <<< "$(cat "$f" 2>/dev/null)"
        [ "$state" = LANDED ] || continue
        git -C "$repo" cat-file -e "${tip}^{commit}" 2>/dev/null || continue
        git -C "$repo" merge-base --is-ancestor "$tip" "$head_sha" 2>/dev/null || continue
        git -C "$repo" merge-base --is-ancestor "$tip" "$forge_sha" 2>/dev/null && continue
        out="${out:+$out }${id}:${tip}"
    done
    printf '%s' "$out"
}

# cmd_publish: see the header comment above (queue.sh publish:). Pushes local/main's
# commits since the last publish (the forge target's own current tip, fetched fresh —
# there is no separate "last published" pointer to drift from it) to a new forge branch
# and opens one PR. Refuses outright, publishing nothing, if the forge target is not an
# ancestor of the local landing ref — something else moved it, and row 4 (the divergence
# alarm) is what raises that to the operator; this only refuses to make it worse.
cmd_publish() {
    local name=""
    if [ "${SPIRA_FAYTH:-}" = czar ] && [ -n "${SPIRA_CZAR_CLASS:-}" ]; then
        bash "$HERE/czar-fence.sh" "$SPIRA_CZAR_CLASS" || return 1
    fi
    while [ $# -gt 0 ]; do
        case "$1" in
        -*) printf 'queue.sh publish: unknown option: %s\n' "$1" >&2; return 2 ;;
        *)  name="$1"; shift ;;
        esac
    done
    [ -n "$name" ] || name="$(spira_home_repo)"

    local repo; repo="$(repo_root "$name" 2>/dev/null)" || {
        printf 'queue.sh publish: no such repo: %s\n' "$name" >&2; return 1
    }
    local mode; mode="$(repo_land "$name")"
    [ "$mode" = "queue.local" ] || {
        printf 'queue.sh publish: repo is not in queue.local mode (mode=%s)\n' "$mode" >&2; return 1
    }

    local base; base="$(spira_landref "$repo")" || {
        printf 'queue.sh publish: cannot resolve landing ref for %s\n' "$name" >&2; return 1
    }
    if ref_remote "$base" "$repo" >/dev/null 2>&1; then
        printf 'queue.sh publish: %s resolves to a remote-tracking ref (%s) — not a queue.local base\n' \
            "$name" "$base" >&2
        return 1
    fi
    local base_branch="$base"

    local target remote forge_branch
    target="$(spira_publish_forge "$name")" || {
        printf 'queue.sh publish: cannot resolve a forge target for %s\n' "$name" >&2; return 1
    }
    read -r remote forge_branch <<< "$target"

    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    if [ "${SPIRA_QUEUE_LOCK_HELD:-0}" != 1 ]; then
        { exec 9>"$lockfile"; } 2>/dev/null \
            || { printf 'queue.sh publish: cannot open lock file for %s\n' "$name" >&2; return 1; }
        if ! flock -n 9; then
            printf 'queue.sh publish: another queue operation holds the lock for %s\n' "$name" >&2
            return 1
        fi
    fi

    local publish_file; publish_file="$(_publish_file "$name")"
    if [ -f "$publish_file" ]; then
        printf 'queue.sh publish: a publish PR is already open for %s (pr=%s) — settle it first\n' \
            "$name" "$(_publish_field pr "$publish_file")" >&2
        return 1
    fi

    git -C "$repo" fetch -q "$remote" "$forge_branch" 2>/dev/null || {
        printf 'queue.sh publish: could not fetch %s/%s\n' "$remote" "$forge_branch" >&2
        return 1
    }
    local forge_sha head_sha
    forge_sha="$(git -C "$repo" rev-parse -q --verify "refs/remotes/$remote/$forge_branch" 2>/dev/null)" || {
        printf 'queue.sh publish: cannot resolve %s/%s\n' "$remote" "$forge_branch" >&2
        return 1
    }
    head_sha="$(git -C "$repo" rev-parse -q --verify "$base_branch" 2>/dev/null)" || {
        printf 'queue.sh publish: cannot resolve %s\n' "$base_branch" >&2
        return 1
    }

    if ! queue_local_check_divergence "$name" "$repo" "$forge_sha" "$head_sha"; then
        printf 'queue.sh publish: %s/%s is not an ancestor of %s — refusing to publish (something else moved the forge)\n' \
            "$remote" "$forge_branch" "$base_branch" >&2
        return 1
    fi

    if [ "$forge_sha" = "$head_sha" ]; then
        printf 'queue.sh publish: nothing to publish for %s (%s/%s already at %s)\n' \
            "$name" "$remote" "$forge_branch" "${head_sha:0:12}"
        return 0
    fi

    local members; members="$(_publish_members "$repo" "$forge_sha" "$head_sha")"
    [ -n "$members" ] || {
        printf 'queue.sh publish: %s has new commits on %s but no LANDED member in range — refusing\n' \
            "$name" "$base_branch" >&2
        return 1
    }

    local stamp branch
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    branch="spira/publish/$stamp"

    if ! spira_git_push "$repo" -q "$remote" "${head_sha}:refs/heads/${branch}" 2>/dev/null; then
        printf 'queue.sh publish: could not push %s\n' "$branch" >&2
        return 1
    fi

    local forge="${SPIRA_FORGE:-$HERE/forge.sh}"
    local member_ids=() _m
    for _m in $members; do member_ids+=("${_m%%:*}"); done
    local member_titles_json
    member_titles_json="$(bdjson show "${member_ids[@]}" 2>/dev/null)" || member_titles_json="[]"

    local pr_body mid t
    pr_body="$(
        printf 'Publish queue: %d bead(s) landed on %s since the last publish, for %s.\n\n' \
            "${#member_ids[@]}" "$base_branch" "$name"
        printf 'These commits already fast-forwarded %s locally; CI here is confirmation, not the gate. A red run files a fix-forward bead — it never rolls back production.\n\n' "$base_branch"
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
            | "$forge" pr-create "$repo" "$branch" "$forge_branch" \
                "publish: ${#member_ids[@]} bead(s) for $name" 2>/dev/null)" || {
        printf 'queue.sh publish: forge pr-create failed for %s\n' "$branch" >&2
        return 1
    }
    [ -n "${pr_n:-}" ] || {
        printf 'queue.sh publish: forge returned no PR number for %s\n' "$branch" >&2
        return 1
    }

    mkdir -p "$(dirname "$publish_file")"
    local opened_at; opened_at="$(date +%s)"
    {
        printf 'pr=%s\n'           "$pr_n"
        printf 'head=%s\n'         "$head_sha"
        printf 'base=%s\n'         "$forge_sha"
        printf 'members=%s\n'      "$members"
        printf 'opened=%s\n'       "$opened_at"
        printf 'branch=%s\n'       "$branch"
        printf 'remote=%s\n'       "$remote"
        printf 'forge_branch=%s\n' "$forge_branch"
    } > "$publish_file"

    printf 'QUEUE PUBLISH %s repo=%s pr=%s members=%d\n' \
        "$opened_at" "$name" "$pr_n" "${#member_ids[@]}" >> "$SPIRA_RUN/landing.log" 2>/dev/null || true

    printf 'queue.sh publish: PR %s opened — %d bead(s) since last publish (%s)\n' \
        "$pr_n" "${#member_ids[@]}" "$branch"
}

# cmd_to_forge: see the header comment above (to-forge/to-local:). Holds the repo's queue
# lock for the whole move — the same lock land-local and publish take — so nothing else can
# cut a local round out from under it. A red or timed-out final publish is left OPEN for the
# ordinary cadence (verdict.sh) to settle; this never runs attribution itself.
cmd_to_forge() {
    local name=""
    while [ $# -gt 0 ]; do
        case "$1" in
        -*) printf 'queue.sh to-forge: unknown option: %s\n' "$1" >&2; return 2 ;;
        *)  name="$1"; shift ;;
        esac
    done
    [ -n "$name" ] || name="$(spira_home_repo)"

    local repo; repo="$(repo_root "$name" 2>/dev/null)" || {
        printf 'queue.sh to-forge: no such repo: %s\n' "$name" >&2; return 1
    }
    local mode; mode="$(repo_land "$name")"
    [ "$mode" = "queue.local" ] || {
        printf 'queue.sh to-forge: %s is not in queue.local mode (mode=%s) — nothing to transition\n' "$name" "$mode" >&2
        return 1
    }
    _land_mode_agrees "$name" || return 1

    local base; base="$(spira_landref "$repo")" || {
        printf 'queue.sh to-forge: cannot resolve landing ref for %s\n' "$name" >&2; return 1
    }
    if ref_remote "$base" "$repo" >/dev/null 2>&1; then
        printf 'queue.sh to-forge: %s resolves to a remote-tracking ref (%s) — not a queue.local base\n' \
            "$name" "$base" >&2
        return 1
    fi
    local base_branch="$base"
    git -C "$repo" show-ref --verify --quiet "refs/heads/$base_branch" || {
        printf 'queue.sh to-forge: %s does not exist as a local branch in %s\n' "$base_branch" "$repo" >&2
        return 1
    }

    local checked_out; checked_out="$(git -C "$repo" symbolic-ref -q --short HEAD 2>/dev/null)"
    [ "$checked_out" != "$base_branch" ] || {
        printf 'queue.sh to-forge: the checkout at %s is on %s — check out a different branch before repointing it\n' \
            "$repo" "$base_branch" >&2
        return 1
    }

    local target remote forge_branch
    target="$(spira_publish_forge "$name")" || {
        printf 'queue.sh to-forge: cannot resolve a forge target for %s\n' "$name" >&2; return 1
    }
    read -r remote forge_branch <<< "$target"

    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    { exec 9>"$lockfile"; } 2>/dev/null \
        || { printf 'queue.sh to-forge: cannot open lock file for %s\n' "$name" >&2; return 1; }
    if ! flock -n 9; then
        printf 'queue.sh to-forge: another queue operation holds the lock for %s — stop cutting local rounds first\n' "$name" >&2
        return 1
    fi

    # Re-read under the lock: a concurrent land-local or a prior transition could have
    # moved the mode between the unlocked checks above and here.
    mode="$(repo_land "$name")"
    [ "$mode" = "queue.local" ] || {
        printf 'queue.sh to-forge: %s is not in queue.local mode (mode=%s) — nothing to transition\n' "$name" "$mode" >&2
        return 1
    }

    printf 'queue.sh to-forge: running the final publish for %s\n' "$name"
    local pub_out pub_rc
    pub_out="$(SPIRA_QUEUE_LOCK_HELD=1 cmd_publish "$name" 2>&1)"; pub_rc=$?
    printf '%s\n' "$pub_out"
    [ "$pub_rc" -eq 0 ] || {
        printf 'queue.sh to-forge: the final publish failed — refused, nothing changed\n' >&2
        return 1
    }

    local publish_file; publish_file="$(_publish_file "$name")"
    if [ -f "$publish_file" ]; then
        # shellcheck disable=SC1090
        . "$HERE/verdict.sh"
        local pr_n; pr_n="$(_publish_field pr "$publish_file")"
        local interval="${SPIRA_QUEUE_TRANSITION_POLLSEC:-5}"
        local deadline=$(( $(date +%s) + ${SPIRA_QUEUE_TRANSITION_MAXSEC:-1800} ))
        printf 'queue.sh to-forge: waiting for publish PR %s to settle green\n' "$pr_n"
        while :; do
            _verdict_settle_publish "$name" "$repo"
            local settle_rc=$?
            if [ "$settle_rc" -eq 3 ]; then
                printf 'queue.sh to-forge: the final publish (PR %s) is red — refused, nothing changed; it is left open for the normal fix-forward recovery\n' \
                    "$pr_n" >&2
                return 1
            fi
            [ -f "$publish_file" ] || break
            if [ "$(date +%s)" -ge "$deadline" ]; then
                printf 'queue.sh to-forge: timed out waiting for publish PR %s to settle — refused, nothing changed\n' "$pr_n" >&2
                return 1
            fi
            sleep "$interval"
        done
    fi

    # THE PRECONDITION THE DESIGN NAMES: origin/main (the forge target, fetched fresh) must
    # be IDENTICAL to local/main, not merely an ancestor — the settle above already made it
    # so on a green publish, but re-verifying rather than trusting that catches anything else
    # that moved either ref in between.
    git -C "$repo" fetch -q "$remote" "$forge_branch" 2>/dev/null || {
        printf 'queue.sh to-forge: could not fetch %s/%s to verify\n' "$remote" "$forge_branch" >&2
        return 1
    }
    local forge_sha local_sha
    forge_sha="$(git -C "$repo" rev-parse -q --verify "refs/remotes/$remote/$forge_branch" 2>/dev/null)"
    local_sha="$(git -C "$repo" rev-parse -q --verify "$base_branch" 2>/dev/null)"
    if [ -z "$forge_sha" ] || [ -z "$local_sha" ] || [ "$forge_sha" != "$local_sha" ]; then
        printf 'queue.sh to-forge: %s/%s (%s) and %s (%s) differ — refused, nothing changed\n' \
            "$remote" "$forge_branch" "${forge_sha:-<none>}" "$base_branch" "${local_sha:-<none>}" >&2
        return 1
    fi

    local new_base="$remote/$forge_branch"
    _land_mode_write_row "$name" "queue.forge" "$new_base" || {
        printf 'queue.sh to-forge: writing the new row failed for %s — reconcile repo-map/spira.toml by hand\n' "$name" >&2
        return 1
    }

    local new_mode new_ref
    new_mode="$(repo_land "$name")"
    new_ref="$(spira_landref "$name" 2>/dev/null)"
    if [ "$new_mode" != "queue" ] || [ "$new_ref" != "$new_base" ]; then
        printf 'queue.sh to-forge: post-write verification failed for %s (mode=%s ref=%s, expected queue at %s) — fix by hand\n' \
            "$name" "$new_mode" "$new_ref" "$new_base" >&2
        return 1
    fi

    local archive_ref="refs/archive/$base_branch"
    git -C "$repo" update-ref "$archive_ref" "$local_sha" 2>/dev/null || true
    git -C "$repo" branch -D "$base_branch" >/dev/null 2>&1 || true

    queue_notify_concierge "$name" "flipped to queue.forge" \
        "$name moved from queue.local to queue.forge; base is now $new_base. $base_branch archived at $archive_ref."

    printf 'queue.sh to-forge: %s is now queue.forge (base=%s); %s archived at %s\n' \
        "$name" "$new_base" "$base_branch" "$archive_ref"
}

# cmd_to_local: the reverse of cmd_to_forge. Re-derives local/<branch> from the forge base —
# reusing the archive cmd_to_forge left behind when one exists there, and refusing unless the
# forge base descends from it — then syncs it to the forge's current tip before flipping the
# row back. A repo that has never been through queue.local has no archive to reuse, so its
# first flip to queue.local simply starts the local branch at the forge's current tip.
cmd_to_local() {
    local name=""
    while [ $# -gt 0 ]; do
        case "$1" in
        -*) printf 'queue.sh to-local: unknown option: %s\n' "$1" >&2; return 2 ;;
        *)  name="$1"; shift ;;
        esac
    done
    [ -n "$name" ] || name="$(spira_home_repo)"

    local repo; repo="$(repo_root "$name" 2>/dev/null)" || {
        printf 'queue.sh to-local: no such repo: %s\n' "$name" >&2; return 1
    }
    local mode; mode="$(repo_land "$name")"
    [ "$mode" = "queue" ] || {
        printf 'queue.sh to-local: %s is not in queue.forge mode (mode=%s) — nothing to transition\n' "$name" "$mode" >&2
        return 1
    }
    _land_mode_agrees "$name" || return 1

    local base; base="$(spira_landref "$repo")" || {
        printf 'queue.sh to-local: cannot resolve landing ref for %s\n' "$name" >&2; return 1
    }
    local remote; remote="$(ref_remote "$base" "$repo")" || {
        printf 'queue.sh to-local: %s does not resolve to a remote-tracking ref — not a queue.forge base\n' "$base" >&2
        return 1
    }
    local forge_branch; forge_branch="$(ref_branch "$base")"
    local new_base_branch="local/$forge_branch"

    local checked_out; checked_out="$(git -C "$repo" symbolic-ref -q --short HEAD 2>/dev/null)"
    [ "$checked_out" != "$new_base_branch" ] || {
        printf 'queue.sh to-local: the checkout at %s is already on %s — check out a different branch first\n' \
            "$repo" "$new_base_branch" >&2
        return 1
    }
    git -C "$repo" show-ref --verify --quiet "refs/heads/$new_base_branch" && {
        printf 'queue.sh to-local: %s already exists as a local branch — refused, nothing changed\n' "$new_base_branch" >&2
        return 1
    }

    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    { exec 9>"$lockfile"; } 2>/dev/null \
        || { printf 'queue.sh to-local: cannot open lock file for %s\n' "$name" >&2; return 1; }
    if ! flock -n 9; then
        printf 'queue.sh to-local: another queue operation holds the lock for %s\n' "$name" >&2
        return 1
    fi

    mode="$(repo_land "$name")"
    [ "$mode" = "queue" ] || {
        printf 'queue.sh to-local: %s is not in queue.forge mode (mode=%s) — nothing to transition\n' "$name" "$mode" >&2
        return 1
    }

    git -C "$repo" fetch -q "$remote" "$forge_branch" 2>/dev/null || {
        printf 'queue.sh to-local: could not fetch %s/%s\n' "$remote" "$forge_branch" >&2
        return 1
    }
    local forge_sha; forge_sha="$(git -C "$repo" rev-parse -q --verify "refs/remotes/$remote/$forge_branch" 2>/dev/null)"
    [ -n "$forge_sha" ] || {
        printf 'queue.sh to-local: cannot resolve %s/%s\n' "$remote" "$forge_branch" >&2
        return 1
    }

    local archive_ref="refs/archive/$new_base_branch" start_sha
    start_sha="$(git -C "$repo" rev-parse -q --verify "$archive_ref" 2>/dev/null)"
    if [ -n "$start_sha" ] && ! git -C "$repo" merge-base --is-ancestor "$start_sha" "$forge_sha" 2>/dev/null; then
        printf 'queue.sh to-local: the archived %s (%s) is not an ancestor of %s/%s (%s) — refs differ, refused\n' \
            "$new_base_branch" "$start_sha" "$remote" "$forge_branch" "$forge_sha" >&2
        return 1
    fi

    git -C "$repo" branch "$new_base_branch" "$forge_sha" 2>/dev/null || {
        printf 'queue.sh to-local: could not create %s at %s\n' "$new_base_branch" "$forge_sha" >&2
        return 1
    }

    if ! _land_mode_write_row "$name" "queue.local" "$new_base_branch"; then
        git -C "$repo" branch -D "$new_base_branch" >/dev/null 2>&1 || true
        printf 'queue.sh to-local: writing the new row failed for %s — reconcile repo-map/spira.toml by hand\n' "$name" >&2
        return 1
    fi

    local new_mode new_ref
    new_mode="$(repo_land "$name")"
    new_ref="$(spira_landref "$name" 2>/dev/null)"
    if [ "$new_mode" != "queue.local" ] || [ "$new_ref" != "$new_base_branch" ]; then
        printf 'queue.sh to-local: post-write verification failed for %s (mode=%s ref=%s, expected queue.local at %s) — fix by hand\n' \
            "$name" "$new_mode" "$new_ref" "$new_base_branch" >&2
        return 1
    fi

    queue_notify_concierge "$name" "flipped to queue.local" \
        "$name moved from queue.forge to queue.local; base is now $new_base_branch, synced to $remote/$forge_branch at $forge_sha."

    printf 'queue.sh to-local: %s is now queue.local (base=%s, synced to %s/%s)\n' \
        "$name" "$new_base_branch" "$remote" "$forge_branch"
}

# cmd_rollback_local: activate the previous round's already-built release and reset
# local/main back to its archived head. THE ONLY UNDO for a round that landed and activated
# cleanly but turned out wrong — it does not touch bead state, which land-local already
# closed against the round now being rolled back from; that is a fact about what shipped and
# stays true regardless of what is running.
cmd_rollback_local() {
    local name="${1:-$(spira_home_repo)}"
    if [ "${SPIRA_FAYTH:-}" = czar ] && [ -n "${SPIRA_CZAR_CLASS:-}" ]; then
        bash "$HERE/czar-fence.sh" "$SPIRA_CZAR_CLASS" || return 1
    fi

    local repo; repo="$(repo_root "$name" 2>/dev/null)" || {
        printf 'queue.sh rollback-local: no such repo: %s\n' "$name" >&2; return 1; }
    local mode; mode="$(repo_land "$name")"
    [ "$mode" = "queue.local" ] || {
        printf 'queue.sh rollback-local: repo is not in queue.local mode (mode=%s)\n' "$mode" >&2; return 1; }

    local base; base="$(spira_landref "$repo")" || {
        printf 'queue.sh rollback-local: cannot resolve landing ref for %s\n' "$name" >&2; return 1; }
    local base_branch="$base"

    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    if [ "${SPIRA_QUEUE_LOCK_HELD:-0}" != 1 ]; then
        { exec 9>"$lockfile"; } 2>/dev/null \
            || { printf 'queue.sh rollback-local: cannot open lock file for %s\n' "$name" >&2; return 1; }
        if ! flock -n 9; then
            printf 'queue.sh rollback-local: another queue operation holds the lock for %s\n' "$name" >&2
            return 1
        fi
    fi

    local seqfile="${SPIRA_QUEUE_DIR:?}/$name/round-seq" n
    n="$(cat "$seqfile" 2>/dev/null)"; case "$n" in ''|*[!0-9]*) n=0 ;; esac
    [ "$n" -ge 2 ] || {
        printf 'queue.sh rollback-local: round-seq is %s — no landed round to roll back to\n' "$n" >&2
        return 1
    }
    local prev_head; prev_head="$(git -C "$repo" rev-parse -q --verify "refs/archive/rounds/$((n-1))" 2>/dev/null)" || {
        printf 'queue.sh rollback-local: refs/archive/rounds/%d has no archived head\n' "$((n-1))" >&2
        return 1
    }

    local tarball="${SPIRA_RELEASES:?}/.tarballs/spira-$prev_head.tar.gz"
    [ -f "$tarball" ] || {
        printf 'queue.sh rollback-local: no retained tarball for the previous release at %s\n' "$tarball" >&2
        return 1
    }

    local act_out act_rc
    act_out="$(bash "$HERE/activate.sh" "$tarball" 2>&1)"; act_rc=$?
    printf '%s\n' "$act_out" >&2
    [ "$act_rc" -eq 0 ] || {
        printf 'queue.sh rollback-local: activate.sh failed to re-activate spira-%s\n' "$prev_head" >&2
        return 1
    }

    local cur_sha; cur_sha="$(git -C "$repo" rev-parse -q --verify "$base_branch" 2>/dev/null)" || cur_sha=""
    if [ -n "$cur_sha" ]; then
        git -C "$repo" update-ref "refs/heads/$base_branch" "$prev_head" "$cur_sha" 2>/dev/null || {
            printf 'queue.sh rollback-local: %s moved concurrently — release rolled back but the ref did not\n' "$base" >&2
            return 1
        }
    else
        git -C "$repo" update-ref "refs/heads/$base_branch" "$prev_head" 2>/dev/null
    fi

    queue_notify_concierge "$name" "local rollback (round $n -> $((n-1)))" \
        "$base reset to $prev_head (release spira-$prev_head re-activated)."

    printf 'queue.sh rollback-local: activated spira-%s, %s reset to %s\n' "$prev_head" "$base" "$prev_head"
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
        publish) shift; cmd_publish "$@" ;;
        to-forge) shift; cmd_to_forge "$@" ;;
        to-local) shift; cmd_to_local "$@" ;;
        rollback-local) shift; cmd_rollback_local "$@" ;;
        *) printf 'usage: queue.sh submit <branch> [<repo>] | queue.sh protect [<repo>] | queue.sh stats | queue.sh flush [<repo>] | queue.sh step <repo> | queue.sh eject <id> [--reason <text>] [--dry-run] [<repo>] | queue.sh abandon [<repo>] --reason <text> [--dry-run] | queue.sh open-batch [<repo>] [--members <ids>] [--skip-pregate] [--dry-run] | queue.sh claim [<repo>] --reason <text> [--force] | queue.sh release [<repo>] | queue.sh land-local [<repo>] --head <sha> --members <id:tip[,id:tip...]> | queue.sh publish [<repo>] | queue.sh to-forge [<repo>] | queue.sh to-local [<repo>] | queue.sh rollback-local [<repo>]\n' >&2; return 2 ;;
    esac
}

# Sourced (a suite wants _lc_cut_batch/_lc_abandon_batch/_lc_eject_member without a real
# dispatch) vs executed: matches batch.sh's own guard, and for the same reason.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    main "$@"
    exit $?
fi
