#!/usr/bin/env bash
# batch.sh — merge-queue batch builder.
# Usage: batch.sh <repo-name>
#
# Opens a batch when the repository has no open batch and either
# SPIRA_QUEUE_BATCH_MAX certified branches exist or the oldest certified
# branch has waited SPIRA_QUEUE_BATCH_WAIT seconds. Branches are ordered:
# suite-state transitions first, then bead priority, then certification time.
# A branch that conflicts with the accumulating batch is skipped (stays
# CERTIFIED); a branch that conflicts with the land ref itself is reopened.
#
# covers: spira/batch.sh spira/conf.sh spira/landing.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"


# format_batch <worktree> <base_sha> [repo-name] -> 0 always; the batch stands
# whatever the formatter does. Same hygiene as format_rebased in lib.sh, applied
# to the tree a batch merge synthesises.
format_batch() {
    local wt="$1" base="$2" name="${3:-}" cmd paths f staged=0

    [ -n "$name" ] || name="$(repo_name_at "$(git -C "$wt" rev-parse --show-toplevel 2>/dev/null)" 2>/dev/null)" || return 0
    cmd="$(repo_format "$name" 2>/dev/null)"
    [ -n "$cmd" ] || return 0

    if ! ( cd "$wt" && env -i PATH="$HOME/.cargo/bin:$PATH" HOME="$HOME" TERM=dumb \
             timeout "${SPIRA_FORMAT_TIMEOUT:-300}" bash -c "$cmd" ) >/dev/null 2>&1; then
        log "format: $name's formatter failed on batch — leaving it unformatted"
        git -C "$wt" checkout -q -- . 2>/dev/null
        return 0
    fi

    paths=()
    while IFS= read -r -d '' f; do
        [ -f "$wt/$f" ] && paths+=("$f")
    done < <(git -C "$wt" diff -z --name-only "$base" HEAD 2>/dev/null)
    [ "${#paths[@]}" -gt 0 ] && git -C "$wt" add -- "${paths[@]}" 2>/dev/null

    git -C "$wt" checkout -q -- . 2>/dev/null
    git -C "$wt" diff --cached --quiet 2>/dev/null || staged=1
    [ "$staged" = 1 ] || return 0

    git -C "$wt" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" commit -q -F - <<EOF 2>/dev/null
spira: format batch

Batch assembly merges branches without re-running $name's formatter, so the
assembled tree may fail the required layout check. Formatted with: $cmd
EOF
    log "format: formatted $name batch"
    return 0
}

# SPIRA_QUEUE_REPRO_BATCH: seam for per-member reproduction in tests.
: "${SPIRA_QUEUE_REPRO_BATCH:=$HERE/testenv-batch.sh}"

_lg_repro_is_red() {   # _lg_repro_is_red <suites-csv> <repo-dir> <branch> -> 0 if red
    local suites="$1" repo="$2" br="$3" tmp rc
    tmp="$(mktemp -d)"
    SPIRA_BATCH_RESULTS="$tmp" bash "$SPIRA_QUEUE_REPRO_BATCH" \
        --mode serial --suites "$suites" "$br" >/dev/null 2>&1
    rc=$?
    rm -rf "$tmp"
    [ "$rc" -eq 1 ]
}

_lg_red_suites() {   # _lg_red_suites <gate-output> -> comma-separated suite names
    printf '%s\n' "$1" | awk '{
        for (i = 1; i < NF; i++)
            if ($i ~ /\.sh$/ && ($(i+1) ~ /^(RED|TIMEOUT|FAILED)$/ ||
                ($(i+1) == "was" && $(i+2) == "killed")))
                if (!seen[$i]++) print $i
    }' | tr '\n' ',' | sed 's/,$//'
}

_batch_open_file() { printf '%s/%s/open' "${SPIRA_QUEUE_DIR:?}" "$1"; }

_batch_is_open() { [ -f "$(_batch_open_file "$1")" ]; }

# _certified_orphans <repo-path> — print id for each CERTIFIED landstate with no branch ref
_certified_orphans() {
    local f id st
    [ -d "$LANDSTATE" ] || return 0
    for f in "$LANDSTATE/"*; do
        [ -f "$f" ] || continue
        id="$(basename "$f")"
        st=""; { read -r st _ < "$f"; } 2>/dev/null || [ -n "$st" ] || continue
        [ "$st" = "CERTIFIED" ] || continue
        git -C "$1" show-ref --verify -q "refs/heads/spira/$id" 2>/dev/null && continue
        printf '%s\n' "$id"
    done
}

# _closed_red_live <repo-path> — print id for each RED/EJECTED landstate whose branch
# exists and whose bead is closed. This is the counterpart to _certified_orphans: where
# that catches CERTIFIED with no branch (deleted while queued), this catches closed with
# a live branch and a failed landstate — the eviction-race shape where a bead ends up
# closed+RED+live and no queue mechanism retrieves it.
_closed_red_live() {
    local f id st _crl_st
    [ -d "$LANDSTATE" ] || return 0
    for f in "$LANDSTATE/"*; do
        [ -f "$f" ] || continue
        id="$(basename "$f")"
        case "$id" in .*|*/*) continue ;; esac
        { read -r st _ < "$f"; } 2>/dev/null || continue
        [ "$st" = "RED" ] || [ "$st" = "EJECTED" ] || continue
        git -C "$1" show-ref --verify -q "refs/heads/spira/$id" 2>/dev/null || continue
        _crl_st="$(bdjson show "$id" 2>/dev/null \
            | python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
d=d if isinstance(d,list) else [d]
if d and d[0].get("status")=="closed": print("closed")' 2>/dev/null)" || _crl_st=""
        [ "${_crl_st:-}" = "closed" ] || continue
        printf '%s\n' "$id"
    done
}

# _certified_list <repo-path> — delegates to queue_certified_list in lib.sh.
# Kept as a local alias so callers inside this file do not need updating.
_certified_list() { queue_certified_list "$@"; }

# _base_conflict <repo> <base-sha> <tip>
# 0 when tip conflicts with base alone (branch must be reopened).
# 1 when tip merges cleanly with base (conflict is only with batch accumulation).
# 2 when the test worktree cannot be created (conservative: treat as no-base-conflict).
_base_conflict() {
    local repo="$1" base="$2" tip="$3" wt rc=1
    wt="$SPIRA_RUN/worktree/.batch-ck-$$"
    git -C "$repo" worktree add -q --detach "$wt" "$base" 2>/dev/null || return 2
    git -C "$wt" merge --no-commit --no-ff "$tip" >/dev/null 2>&1 || rc=0
    git -C "$wt" merge --abort 2>/dev/null || true
    git -C "$repo" worktree remove -f "$wt" 2>/dev/null || true
    return $rc
}

main() {
    local name="${1:-}"
    [ -n "$name" ] || { printf 'batch.sh: repo name required\n' >&2; exit 1; }

    local repo base base_sha remote base_branch mode
    repo="$(repo_root "$name")" || { printf 'batch %s: no repo-map entry\n' "$name" >&2; return 1; }
    mode="$(repo_land "$name")"
    [ "$mode" = "queue" ] || return 0

    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    { exec 9>"$lockfile"; } 2>/dev/null \
        || { printf 'batch %s: cannot open lock file\n' "$name" >&2; return 1; }
    if ! flock -n 9; then
        printf 'batch %s: another queue operation holds the lock\n' "$name"
        return 0
    fi

    base="$(spira_landref "$repo")" \
        || { printf 'batch %s: cannot resolve base ref\n' "$name" >&2; return 1; }
    base_sha="$(git -C "$repo" rev-parse "$base" 2>/dev/null)" \
        || { printf 'batch %s: cannot resolve %s\n' "$name" "$base" >&2; return 1; }
    remote="$(ref_remote "$base")"
    base_branch="$(ref_branch "$base")"

    # CERTIFIED landstate records with no branch ref were deleted while queued.
    # batch.sh would skip them silently; log and mail the operator instead.
    local _orphan _orphans _orphan_list=""
    _orphans="$(_certified_orphans "$repo")"
    if [ -n "${_orphans:-}" ]; then
        while IFS= read -r _orphan; do
            [ -n "$_orphan" ] || continue
            printf 'batch %s: WARN certified-orphan %s — CERTIFIED landstate but branch spira/%s is gone\n' \
                "$name" "$_orphan" "$_orphan"
            _orphan_list="${_orphan_list}- ${_orphan}\n"
        done <<< "$_orphans"
        printf '## Note\nBranch(es) were CERTIFIED in the merge queue for %s but their refs are gone:\n\n%b\nThe next batch will not include them. Check the reap log for what deleted the ref.\n' \
            "$name" "$_orphan_list" \
        | bash "$HERE/mail.sh" send operator \
            --from "Spira Queue <queue@spira>" \
            --subject "Merge queue: $name — CERTIFIED branch(es) missing" \
            2>/dev/null || true
    fi

    # Closed beads with RED/EJECTED landstates and live branches are unreachable: the batch
    # builder ignores closed beads and queue_certified_list selects on CERTIFIED. Log and
    # mail the operator so the loss is visible before a "queue drained" conclusion stands.
    local _crl_id _crl_ids _crl_list=""
    _crl_ids="$(_closed_red_live "$repo")"
    if [ -n "${_crl_ids:-}" ]; then
        while IFS= read -r _crl_id; do
            [ -n "$_crl_id" ] || continue
            printf 'batch %s: WARN closed-red-live %s — bead closed with landstate RED/EJECTED and branch spira/%s is alive\n' \
                "$name" "$_crl_id" "$_crl_id"
            _crl_list="${_crl_list}- ${_crl_id}\n"
        done <<< "$_crl_ids"
        printf '## Note\nBead(s) for %s are closed with a RED/EJECTED landstate and a live branch:\n\n%bThese beads cannot re-enter the queue. Re-open and recertify each branch to resume.\n' \
            "$name" "$_crl_list" \
        | bash "$HERE/mail.sh" send operator \
            --from "Spira Queue <queue@spira>" \
            --subject "Merge queue: $name — closed bead(s) with live evicted branch" \
            2>/dev/null || true
    fi

    # Mark CERTIFIED records whose branches are gone so they drop from the queue view.
    # This runs before the open-batch guard so stale records are converged even while a
    # batch PR is pending — without this, a long CI run leaves them accumulating
    # indefinitely (the guard returned early before this block ever ran).
    if [ -d "$LANDSTATE" ]; then
        local _lf _lid _lst _ltip _anyrn _rn _rp
        for _lf in "$LANDSTATE"/*; do
            [ -f "$_lf" ] || continue
            _lid="$(basename "$_lf")"
            # Skip entries that are not bead IDs (no slashes, no leading dot).
            case "$_lid" in .*|*/*) continue ;; esac
            _lst=""; _ltip=""; { read -r _lst _ltip _ < "$_lf"; } 2>/dev/null || [ -n "$_lst" ] || continue
            [ "$_lst" = "CERTIFIED" ] || continue
            _anyrn=0
            for _rn in $(spira_repos); do
                _rp="$(repo_root "$_rn" 2>/dev/null)" || continue
                git -C "$_rp" show-ref --verify --quiet "refs/heads/spira/$_lid" 2>/dev/null \
                    && { _anyrn=1; break; }
            done
            if [ "$_anyrn" = 1 ]; then
                # Branch still exists. Reconcile to LANDED when the certified tip is already
                # in the base AND the branch hasn't moved past it. If the branch has advanced
                # since certification, defer to the _certified_list stale-cert path (inside
                # the open-batch guard), which re-certifies with the live tip first.
                _lcur="$(git -C "$repo" rev-parse "refs/heads/spira/$_lid" 2>/dev/null || true)"
                if [ "${_lcur:-none}" = "${_ltip:-none}" ] && \
                   git -C "$repo" merge-base --is-ancestor "${_ltip:-none}" "$base_sha" \
                       2>/dev/null; then
                    land_mark "$_lid" LANDED "${_ltip:-none}" already-in-base
                    printf 'batch %s: %s tip already in %s (live branch) — LANDED\n' \
                        "$name" "$_lid" "$base"
                fi
                continue
            fi
            # Tip already in base: the branch landed (via external merge before verdict.sh
            # ran). Mark LANDED rather than LOST so the queue view and queue-wait logic
            # both see it as done — LOST drops it from the view but does not unblock
            # queue-waiters that tested for CERTIFIED reaching LANDED.
            if git -C "$repo" merge-base --is-ancestor "${_ltip:-none}" "$base_sha" \
                   2>/dev/null; then
                land_mark "$_lid" LANDED "${_ltip:-none}" already-in-base-orphan
                printf 'batch %s: %s tip already in %s (orphan) — LANDED\n' \
                    "$name" "$_lid" "$base"
            else
                # The Sending reaps branches it verified as landed/superseded; a REMOVED
                # entry here means the content landed even if the tip is not an ancestor
                # (rebased or squash-merged). Without an entry, the deletion was unexpected.
                if [ -f "${SPIRA_REAPLOG:-$SPIRA_RUN/reap.log}" ] && \
                       awk -v id="$_lid" '$2 == "REMOVED" && $3 == id {found=1} END {exit !found}' \
                           "${SPIRA_REAPLOG:-$SPIRA_RUN/reap.log}" 2>/dev/null; then
                    land_mark "$_lid" LANDED "${_ltip:-none}" reaped-orphan
                    printf 'batch %s: %s has no branch — LANDED (reaped-orphan)\n' "$name" "$_lid"
                else
                    land_mark "$_lid" LOST "${_ltip:-none}" branch-gone
                    printf 'batch %s: %s has no branch — LOST (branch-gone)\n' "$name" "$_lid"
                fi
            fi
        done
    fi

    if _batch_is_open "$name"; then
        local _ob_forge="${SPIRA_FORGE:-$HERE/forge.sh}"
        local _ob_file; _ob_file="$(_batch_open_file "$name")"
        local _ob_pr
        _ob_pr="$(grep '^pr=' "$_ob_file" 2>/dev/null | head -1)"; _ob_pr="${_ob_pr#pr=}"
        if [ -n "$_ob_pr" ]; then
            local _ob_mstat
            _ob_mstat="$("$_ob_forge" pr-mergeability "$repo" "$_ob_pr" 2>/dev/null)" \
                || _ob_mstat="UNKNOWN"
            if [ "${_ob_mstat:-UNKNOWN}" = "DIRTY" ]; then
                printf 'batch %s: PR %s is DIRTY (merge conflicts) — abandoning\n' \
                    "$name" "$_ob_pr"
                local _ob_members _ob_m _ob_mid _ob_mtip _ob_cur
                _ob_members="$(grep '^members=' "$_ob_file" 2>/dev/null | head -1)"
                _ob_members="${_ob_members#members=}"
                for _ob_m in $_ob_members; do
                    _ob_mid="${_ob_m%%:*}"; _ob_mtip="${_ob_m##*:}"
                    _ob_cur=""
                    [ -f "$LANDSTATE/$_ob_mid" ] && \
                        { read -r _ob_cur _ < "$LANDSTATE/$_ob_mid" 2>/dev/null || true; }
                    case "${_ob_cur:-}" in
                    RED|EJECTED)
                        printf 'batch %s: %s left at %s\n' "$name" "$_ob_mid" "$_ob_cur" ;;
                    *)
                        land_mark "$_ob_mid" CERTIFIED "$_ob_mtip"
                        printf 'batch %s: %s returned to CERTIFIED\n' "$name" "$_ob_mid" ;;
                    esac
                done
                "$_ob_forge" pr-comment "$repo" "$_ob_pr" \
                    "Batch abandoned: PR had merge conflicts (DIRTY). Members returned to CERTIFIED for re-batching." \
                    2>/dev/null || true
                "$_ob_forge" pr-close "$repo" "$_ob_pr" 2>/dev/null || true
                local _ob_stamp; _ob_stamp="$(date -u +%Y%m%dT%H%M%SZ)"
                mv "$_ob_file" \
                    "$(dirname "$_ob_file")/closed-pr${_ob_pr}-${_ob_stamp}" \
                    2>/dev/null || rm -f "$_ob_file"
                printf '## Note\nBatch PR %s for %s was found unmergeable (DIRTY) and has been abandoned.\n\nMembers returned to CERTIFIED and will be re-batched on the next pass.\n' \
                    "$_ob_pr" "$name" \
                | bash "$HERE/mail.sh" send operator \
                    --from "Spira Queue <queue@spira>" \
                    --subject "Merge queue: $name — batch PR abandoned (conflicts)" \
                    2>/dev/null || true
                return 0
            fi
        fi
        printf 'batch %s: open batch exists — skipping\n' "$name"
        return 0
    fi

    local certs
    certs="$(_certified_list "$repo")"

    # Mark already-in-base certified tips LANDED so they do not consume batch slots
    # or inflate the wait trigger.
    if [ -n "${certs:-}" ]; then
        local _filt="" _cid _ctip _cepoch _ltip
        while IFS= read -r _cl; do
            [ -n "$_cl" ] || continue
            read -r _cid _ctip _cepoch <<< "$_cl"
            _ltip="$(git -C "$repo" rev-parse "refs/heads/spira/$_cid" 2>/dev/null || true)"
            if [ -n "${_ltip:-}" ] && [ "$_ltip" != "$_ctip" ]; then
                land_mark "$_cid" CERTIFIED "$_ltip"
                printf 'batch %s: stale-certification %s — certified=%s live=%s — re-certified\n' \
                    "$name" "$_cid" "${_ctip:0:8}" "${_ltip:0:8}"
                if git -C "$repo" merge-base --is-ancestor "$_ltip" "$base_sha" 2>/dev/null; then
                    land_mark "$_cid" LANDED "$_ltip" already-in-base
                    printf 'batch %s: %s live tip already in %s — LANDED (already-in-base)\n' \
                        "$name" "$_cid" "$base"
                else
                    _filt="${_filt}${_cid} ${_ltip} ${_cepoch}"$'\n'
                fi
            elif git -C "$repo" merge-base --is-ancestor "$_ctip" "$base_sha" 2>/dev/null; then
                land_mark "$_cid" LANDED "$_ctip" already-in-base
                printf 'batch %s: %s tip already in %s — LANDED (already-in-base)\n' \
                    "$name" "$_cid" "$base"
            else
                _filt="${_filt}${_cl}"$'\n'
            fi
        done <<< "$certs"
        certs="${_filt%$'\n'}"
    fi

    if [ -z "${certs:-}" ]; then
        rm -f "$SPIRA_RUN/queue-stuck-$name" 2>/dev/null || true
        return 0
    fi

    local count now oldest_epoch age triggered=
    count="$(printf '%s\n' "$certs" | grep -c .)"
    now="$(date +%s)"
    oldest_epoch="$(printf '%s\n' "$certs" | awk '{print $3}' | sort -n | head -1)"
    age=$(( now - oldest_epoch ))

    # Measure time since the queue last made progress (BATCHED or LANDED), not the
    # age of the oldest waiting branch. A deep but draining queue has old certs
    # yet recent movement; measuring the cert age alone fires on depth, not stall.
    local last_moved=0 _mf _ms _mt _me
    if [ -d "$LANDSTATE" ]; then
        for _mf in "$LANDSTATE"/*; do
            [ -f "$_mf" ] || continue
            # land_mark omits trailing newline; read returns non-zero at EOF (watchtower.sh:218).
            _ms=""; _mt=""; _me=""
            read -r _ms _mt _me _ < "$_mf" 2>/dev/null || true
            case "$_ms" in BATCHED|LANDED) : ;; *) continue ;; esac
            case "${_me:-}" in ''|*[!0-9]*) continue ;; esac
            [ "$_me" -gt "$last_moved" ] && last_moved="$_me"
        done
    fi

    local _stuck_flag="$SPIRA_RUN/queue-stuck-$name"
    if [ "$last_moved" -eq 0 ]; then
        printf 'batch %s: no BATCHED/LANDED record — stuck check skipped\n' "$name"
    else
        local stuck_age
        stuck_age=$(( now - last_moved ))
        if [ "$stuck_age" -lt "${SPIRA_QUEUE_STUCK_AGE:-7200}" ]; then
            rm -f "$_stuck_flag" 2>/dev/null || true
        elif [ ! -f "$_stuck_flag" ]; then
            printf '## Note\nThe merge queue for %s has not made progress in %ds (threshold %ds).\n\nQueue depth: %d branch(es). This may indicate a conflict loop or a stalled batch builder.\n' \
                "$name" "$stuck_age" "${SPIRA_QUEUE_STUCK_AGE:-7200}" "$count" \
            | bash "$HERE/mail.sh" send operator \
                --from "Spira Queue <queue@spira>" \
                --subject "Merge queue: $name queue stuck (${stuck_age}s)" \
                2>/dev/null && touch "$_stuck_flag" 2>/dev/null || true
            [ -f "$_stuck_flag" ] && \
                printf 'batch %s: mailed operator about stuck queue (stuck_age %ds)\n' "$name" "$stuck_age"
        fi
    fi

    [ "$count" -ge "${SPIRA_QUEUE_BATCH_MAX:-8}" ] && triggered=1
    [ "$age"   -ge "${SPIRA_QUEUE_BATCH_WAIT:-1800}" ] && triggered=1

    # THIRD TRIGGER: CI IS IDLE, SO WAITING BUYS NOTHING. The wait exists to let certified
    # branches accumulate into one CI run instead of spending a run each. That trade is only
    # worth making while a run is in flight — with nothing in CI, a branch that waits its
    # full SPIRA_QUEUE_BATCH_WAIT is a branch held back from an idle machine for no gain.
    # The operator, verbatim (2026-09-23): "if there's NOTHING in CI, then we may as well
    # just send an immediate batch."
    #
    # ASKED LAST, AND ONLY WHEN IT CAN CHANGE THE ANSWER. This is a network round trip and
    # this function runs on every landing pass, so it is reached only when neither cheap
    # trigger fired and there is certified work waiting. When the other two already said yes,
    # the answer cannot matter.
    #
    # ? IS NOT ZERO. forge.sh runs-active prints ? when it cannot tell, and ? must never cut
    # a batch: reading a failed API call as "CI is idle" would fire this trigger on every
    # pass exactly when the forge is unreachable (law-absence-needs-a-positive-control).
    if [ -z "$triggered" ] && [ "${SPIRA_QUEUE_BATCH_IDLE_CUT:-1}" = 1 ] && [ "$count" -gt 0 ]; then
        local _active
        _active="$("${SPIRA_FORGE:-$HERE/forge.sh}" runs-active "$repo" 2>/dev/null)" || _active="?"
        case "${_active:-?}" in
            0) triggered=1
               printf 'batch %s: CI idle (0 runs queued or in progress) — cutting %d certified branch(es) without waiting\n' \
                   "$name" "$count" ;;
            ''|*[!0-9]*) : ;;   # ? or anything unparseable: assume busy, wait it out
        esac
    fi

    [ -n "$triggered" ] || return 0

    # Collect IDs for a bulk priority query.
    local all_ids=()
    while read -r _id _ _; do all_ids+=("$_id"); done <<< "$certs"
    local prio_json
    prio_json="$(bdjson show "${all_ids[@]}" 2>/dev/null)" || prio_json="[]"

    # Sort: suite-transition first, then priority asc, then epoch asc.
    # queue_sort_rows (lib.sh) is the canonical implementation shared with the cockpit.
    local sortfile; sortfile="$(mktemp)"
    # shellcheck disable=SC2064
    trap "rm -f '$sortfile'" RETURN
    PRIO_JSON="$prio_json" queue_sort_rows "$repo" "$base_sha" \
        < <(printf '%s\n' "$certs") > "$sortfile"

    # Build batch in a worktree starting at the land ref.
    local wt
    wt="$SPIRA_RUN/worktree/.batch-$(basename "$repo")"
    # Prune stale registrations (e.g. a directory removed without worktree remove).
    git -C "$repo" worktree prune 2>/dev/null || true
    if [ -e "$wt/.git" ]; then
        git -C "$wt" reset -q --hard "$base_sha" 2>/dev/null
        git -C "$wt" clean -qfd 2>/dev/null || true
    else
        mkdir -p "$(dirname "$wt")"
        git -C "$repo" worktree add -q --detach "$wt" "$base_sha" 2>/dev/null \
            || { printf 'batch %s: cannot create batch worktree\n' "$name" >&2; return 1; }
    fi

    local max="${SPIRA_QUEUE_BATCH_MAX:-8}" taken=0
    local members=() member_ids=()
    local _bid _btip

    while IFS= read -r _line && [ "$taken" -lt "$max" ]; do
        read -r _ _ _ _bid _btip <<< "$_line"
        # Warn when the branch head has moved past the certified tip (sp-hm2vw). The
        # batch uses the certified tip; commits pushed after certification are not included
        # until the branch is re-certified.
        local _bcur
        _bcur="$(git -C "$repo" rev-parse "refs/heads/spira/$_bid" 2>/dev/null || true)"
        if [ -n "${_bcur:-}" ] && [ "$_bcur" != "$_btip" ]; then
            printf 'batch %s: WARN %s tip has moved since certification — certified=%s head=%s — using certified tip\n' \
                "$name" "$_bid" "${_btip:0:8}" "${_bcur:0:8}"
        fi
        unset _bcur
        if git -C "$wt" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" merge --no-edit --no-ff -m "spira: land $_bid" "$_btip" \
               >/dev/null 2>&1; then
            members+=("$_bid:$_btip")
            member_ids+=("$_bid")
            taken=$(( taken + 1 ))
        else
            git -C "$wt" merge --abort 2>/dev/null || true
            if _base_conflict "$repo" "$base_sha" "$_btip"; then
                local _cited_result="" _cited_sha="" _cited_rule="" _unlanded_ahead=""
                _cited_result="$(bead_cited_commit_on_base "$_bid" "$repo" "$base_sha" 2>/dev/null)" || true
                read -r _cited_sha _cited_rule <<< "$_cited_result"
                if [ -n "$_cited_sha" ]; then
                    _unlanded_ahead="$(git -C "$repo" rev-list "$base_sha..$_btip" 2>/dev/null)" \
                        || _unlanded_ahead=""
                    if [ -z "$_unlanded_ahead" ] \
                       || content_landed "$repo" "$_btip" "$base_sha" 2>/dev/null; then
                        land_mark "$_bid" LANDED "$_cited_sha" "${_cited_rule}-complete"
                        printf 'batch %s: %s notes cite %s (%s) already on %s — marked landed\n' \
                            "$name" "$_bid" "$_cited_sha" "${_cited_rule}-complete" "$base"
                    else
                        printf 'batch %s: %s notes cite %s (%s) on %s but branch has unlanded commits — reopening\n' \
                            "$name" "$_bid" "$_cited_sha" "$_cited_rule" "$base"
                        _cited_sha=""
                    fi
                fi
                if [ -z "$_cited_sha" ]; then
                    # Attempt rebase onto base before reopening.
                    local _rbwt _rbtip _rbtmp _rbrc _rbsrc _rbfp _rbconf _wtconf
                    _rbwt="$SPIRA_RUN/worktree/.batch-rb-$$"
                    _rbtip="" _rbrc=1 _rbconf=""
                    if git -C "$repo" worktree add -q --detach "$_rbwt" "$_btip" 2>/dev/null; then
                        _rbfp="$(git -C "$_rbwt" merge-base HEAD "$base_sha" 2>/dev/null)" || _rbfp=""
                        if [ -n "$_rbfp" ]; then
                            git -C "$_rbwt" rebase --onto "$base_sha" "$_rbfp" \
                                >/dev/null 2>&1; _rbrc=$?
                            if [ "$_rbrc" -eq 0 ]; then
                                _rbtip="$(git -C "$_rbwt" rev-parse HEAD 2>/dev/null)"
                            else
                                _rbconf="$(git -C "$_rbwt" diff --name-only --diff-filter=U 2>/dev/null | tr '\n' ' ')"
                                _rbconf="${_rbconf% }"
                                git -C "$_rbwt" rebase --abort 2>/dev/null || true
                            fi
                        fi
                        git -C "$repo" worktree remove -f "$_rbwt" 2>/dev/null || true
                    fi
                    if [ "$_rbrc" -eq 0 ] && [ -n "$_rbtip" ]; then
                        git -C "$repo" branch -f "spira/$_bid" "$_rbtip" 2>/dev/null || true
                        _rbtmp="$(mktemp -d)"
                        SPIRA_BATCH_RESULTS="$_rbtmp" bash "$SPIRA_QUEUE_REPRO_BATCH" \
                            --mode serial "spira/$_bid" >/dev/null 2>&1; _rbsrc=$?
                        rm -rf "$_rbtmp"
                        if [ "$_rbsrc" -eq 0 ]; then
                            land_mark "$_bid" CERTIFIED "$_rbtip"
                            if git -C "$wt" merge --no-edit --no-ff \
                                   -m "spira: land $_bid" "$_rbtip" >/dev/null 2>&1; then
                                members+=("$_bid:$_rbtip")
                                member_ids+=("$_bid")
                                taken=$(( taken + 1 ))
                                printf 'batch %s: %s rebased onto %s — batched\n' \
                                    "$name" "$_bid" "$base"
                            else
                                _wtconf="$(git -C "$wt" diff --name-only --diff-filter=U 2>/dev/null | tr '\n' ' ')"
                                _wtconf="${_wtconf% }"
                                git -C "$wt" merge --abort 2>/dev/null || true
                                bump_requeue "$_bid" merge-conflict >/dev/null 2>&1 || true
                                bead_reopen "$_bid" rebase-conflict \
                                    "$(conflict_reopen_note "$repo" "spira/$_bid" "$base" "$name" "$_wtconf" "batch builder")" \
                                    >/dev/null 2>&1 || true
                                land_mark "$_bid" RED "$_rbtip" conflicts-with-base
                                printf 'batch %s: %s conflicts with %s after rebase — reopened\n' \
                                    "$name" "$_bid" "$base"
                            fi
                        else
                            bump_requeue "$_bid" merge-conflict >/dev/null 2>&1 || true
                            bead_reopen "$_bid" rebase-conflict \
                                "Reopened by batch builder: branch spira/$_bid failed suites after rebase in $name." \
                                >/dev/null 2>&1 || true
                            land_mark "$_bid" RED "$_rbtip" rebase-suite-red
                            printf 'batch %s: %s suite-red after rebase — reopened\n' \
                                "$name" "$_bid"
                        fi
                    else
                        bump_requeue "$_bid" merge-conflict >/dev/null 2>&1 || true
                        bead_reopen "$_bid" rebase-conflict \
                            "$(conflict_reopen_note "$repo" "spira/$_bid" "$base" "$name" "$_rbconf" "batch builder")" \
                            >/dev/null 2>&1 || true
                        land_mark "$_bid" RED "$_btip" conflicts-with-base
                        printf 'batch %s: %s conflicts with %s — reopened\n' "$name" "$_bid" "$base"
                    fi
                    unset _rbwt _rbtip _rbtmp _rbrc _rbsrc _rbfp _rbconf _wtconf
                fi
                unset _cited_result _cited_sha _cited_rule _unlanded_ahead
            else
                # Clean merge with the land ref: conflict is only with batch accumulation — skip.
                printf 'batch %s: %s conflicts with batch — skipped\n' "$name" "$_bid"
            fi
        fi
    done < "$sortfile"

    [ "${#members[@]}" -gt 0 ] || return 0

    local stamp batch_br batch_head
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    batch_br="spira/queue/$stamp"
    format_batch "$wt" "$base_sha" "$name"
    batch_head="$(git -C "$wt" rev-parse HEAD 2>/dev/null)"

    # LOCAL GATE: run the gate on the combined batch before opening a PR.
    # Green → push + open PR. Red → attribute, eject, rebuild, gate again.
    local lg_out lg_rc lg_start lg_cost lg_ejected="" lg_suites_csv=""
    lg_start="$(date +%s)"
    git -C "$repo" branch -f "$batch_br" "$batch_head" 2>/dev/null || true
    # SPIRA_QUEUE_LOCAL_GATE=0 opens the PR without the local gate. The gate runs inside
    # the landing pass, so every batch paid 30-45 min of it -- with no verdict, no other
    # batch and no landing moving -- before CI ran the same corpus (sp-hrkwa).
    if [ "${SPIRA_QUEUE_LOCAL_GATE:-1}" = 0 ]; then
        lg_out=""; lg_rc=0; lg_cost=0
        printf 'batch %s: local gate skipped (SPIRA_QUEUE_LOCAL_GATE=0) — CI is the authority\n' "$name"
    else
        lg_out="$(SPIRA_GATE_BEAD="batch-$stamp" bash "$HERE/gate.sh" "$batch_br" "$name" 2>&1)"
        lg_rc=$?
        lg_cost=$(( $(date +%s) - lg_start ))
    fi

    if [ "$lg_rc" -ne 0 ] && spira_gate_blames_branch "$lg_rc"; then
        local lg_attr_start lg_attr_cost lg_ejected lg_suites_csv
        lg_attr_start="$(date +%s)"
        lg_ejected=""
        lg_suites_csv="$(_lg_red_suites "$lg_out")"

        local lg_survivors=() lg_ejected_arr=() _lmm _lmid _lmtip
        for _lmm in "${members[@]}"; do
            _lmid="${_lmm%%:*}"; _lmtip="${_lmm##*:}"
            if _lg_repro_is_red "${lg_suites_csv:-}" "$repo" "spira/$_lmid"; then
                lg_ejected_arr+=("$_lmm")
            else
                lg_survivors+=("$_lmm")
            fi
        done

        for _lmm in "${lg_ejected_arr[@]:-}"; do
            [ -n "$_lmm" ] || continue
            _lmid="${_lmm%%:*}"; _lmtip="${_lmm##*:}"
            bead_reopen "$_lmid" batch-eject \
                "Ejected by local batch gate: spira/$_lmid reproduced failure in $name." \
                >/dev/null 2>&1 || true
            land_mark "$_lmid" EJECTED "$_lmtip"
            printf 'QUEUE CAUGHT %s branch=%s\n' "$(date +%s)" "$_lmid" \
                >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
            lg_ejected="$lg_ejected${lg_ejected:+ }$_lmid"
            printf 'batch %s: ejected %s — reproduced local gate failure\n' "$name" "$_lmid"
        done

        lg_attr_cost=$(( $(date +%s) - lg_attr_start ))
        printf 'QUEUE BATCH %s repo=%s members=%d gate_seconds=%d verdict=red attr_seconds=%d ejected=%s\n' \
            "$(date +%s)" "$name" "${#members[@]}" "$lg_cost" "$lg_attr_cost" \
            "${lg_ejected:--}" \
            >> "$SPIRA_RUN/landing.log" 2>/dev/null || true

        # ATTRIBUTION FOUND NOBODY — OPEN THE PR ANYWAY.
        #
        # Everything below rebuilds the batch from the survivors and gates it
        # again. When nothing was ejected, survivors IS members, so the "rebuilt"
        # batch re-merges the same commits onto the same base and produces a tree
        # byte-identical to the one that just failed. Gating it again is a
        # guaranteed-identical result, and at the end the members are marked
        # CERTIFIED and the branch deleted — so the next pass assembles exactly the
        # same batch and does it all over. On 2026-09-17 that ran two full gates
        # (~380s) per landing pass, indefinitely, while the queue drained nothing:
        # every batch logged `ejected=-` and no pull request was ever opened.
        #
        # AND A RED NOBODY REPRODUCES IS NOT EVIDENCE AGAINST ANY MEMBER. CI is the
        # authority for a batch (law-green-prs-merge-themselves); this gate buys
        # latency, not coverage (law-local-gates-buy-latency-not-coverage), and
        # leaving it able to veto means a failure it cannot attribute kills a batch
        # CI never sees. Let CI adjudicate.
        #
        # This is deliberately not a fix for sp-2f51e, which is WHY attribution
        # finds nobody — testenv-batch.sh runs against the production checkout
        # rather than the branch it is given, so every member reproduces
        # identically. This guard is correct on its own terms and stays correct
        # once that lands: with working attribution, a red the members genuinely do
        # not carry still belongs to CI.
        if [ "${#lg_ejected_arr[@]}" -eq 0 ]; then
            printf 'batch %s: local gate red (%s) but no member reproduced it — opening the PR; CI is the authority\n' \
                "$name" "${lg_suites_csv:-unattributed}"
        else
            if [ "${#lg_survivors[@]}" -eq 0 ]; then
                SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$batch_br" 2>/dev/null || true
                return 0
            fi

            # Rebuild batch from survivors.
            members=(); member_ids=()
            git -C "$wt" reset -q --hard "$base_sha" 2>/dev/null
            git -C "$wt" clean -qfd 2>/dev/null || true
            for _lmm in "${lg_survivors[@]}"; do
                _lmid="${_lmm%%:*}"; _lmtip="${_lmm##*:}"
                if git -C "$wt" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" merge --no-edit --no-ff -m "spira: land $_lmid" "$_lmtip" \
                       >/dev/null 2>&1; then
                    members+=("$_lmid:$_lmtip")
                    member_ids+=("$_lmid")
                fi
            done

            if [ "${#members[@]}" -eq 0 ]; then
                SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$batch_br" 2>/dev/null || true
                return 0
            fi

            # Delete old batch branch; new stamp for the rebuilt batch.
            SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$batch_br" 2>/dev/null || true
            format_batch "$wt" "$base_sha" "$name"
            batch_head="$(git -C "$wt" rev-parse HEAD 2>/dev/null)"
            stamp="$(date -u +%Y%m%dT%H%M%SZ)"
            batch_br="spira/queue/$stamp"

            # Gate the rebuilt batch.
            lg_start="$(date +%s)"
            git -C "$repo" branch -f "$batch_br" "$batch_head" 2>/dev/null || true
            lg_out="$(SPIRA_GATE_BEAD="batch-$stamp" bash "$HERE/gate.sh" "$batch_br" "$name" 2>&1)"
            lg_rc=$?
            lg_cost=$(( $(date +%s) - lg_start ))

            if [ "$lg_rc" -ne 0 ] && spira_gate_blames_branch "$lg_rc"; then
                for _lmm in "${lg_survivors[@]}"; do
                    _lmid="${_lmm%%:*}"; _lmtip="${_lmm##*:}"
                    land_mark "$_lmid" CERTIFIED "$_lmtip"
                done
                SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$batch_br" 2>/dev/null || true
                printf 'QUEUE BATCH %s repo=%s members=%d gate_seconds=%d verdict=red\n' \
                    "$(date +%s)" "$name" "${#members[@]}" "$lg_cost" \
                    >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
                printf 'batch %s: rebuilt batch also red — no PR opened\n' "$name"
                return 0
            fi
        fi
    fi

    # GREEN: log the batch meter line, push, open PR.
    printf 'QUEUE BATCH %s repo=%s members=%d gate_seconds=%d verdict=green\n' \
        "$(date +%s)" "$name" "${#members[@]}" "${lg_cost:-0}" \
        >> "$SPIRA_RUN/landing.log" 2>/dev/null || true

    local forge="${SPIRA_FORGE:-$HERE/forge.sh}"

    local _oqprs
    _oqprs="$("$forge" pr-list-queue "$repo" 2>/dev/null)" || _oqprs=""
    if [ -n "${_oqprs:-}" ]; then
        local _oqflag="$SPIRA_RUN/queue-unrecorded-pr-$name"
        if [ ! -f "$_oqflag" ]; then
            printf '## Note\nAn open spira/queue/* PR exists for %s that is not in the batch record.\n\nThis indicates concurrent batch builds raced. Inspect and close any orphan queue PR for %s.\n' \
                "$name" "$name" \
            | bash "$HERE/mail.sh" send operator \
                --from "Spira Queue <queue@spira>" \
                --subject "Merge queue: $name — unrecorded open queue PR" \
                2>/dev/null && touch "$_oqflag" 2>/dev/null || true
        fi
        printf 'batch %s: open queue PR not in batch record — skipping\n' "$name"
        return 0
    fi

    if ! git -C "$repo" push -q "$remote" \
           "${batch_head}:refs/heads/${batch_br}" 2>/dev/null; then
        printf 'batch %s: could not push %s\n' "$name" "$batch_br" >&2
        return 1
    fi

    # Open PR via forge seam.
    local member_titles_json
    member_titles_json="$(bdjson show "${member_ids[@]}" 2>/dev/null)" || member_titles_json="[]"

    local pr_body pr_n
    pr_body="$(
        printf 'Merge-queue batch: %d beads for %s, onto %s.\n\n' \
            "${#members[@]}" "$name" "$base_branch"
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
    pr_n="$(printf '%s' "$pr_body" \
            | "$forge" pr-create "$repo" "$batch_br" "$base_branch" \
                "queue: ${#members[@]} beads for $name" 2>/dev/null)" || {
        printf 'batch %s: forge pr-create failed for %s\n' "$name" "$batch_br" >&2
        return 1
    }
    [ -n "${pr_n:-}" ] || {
        printf 'batch %s: forge returned no PR number for %s\n' "$name" "$batch_br" >&2
        return 1
    }

    # Post a comment for each ejected member so the PR body stays accurate.
    if [ -n "${lg_ejected:-}" ]; then
        local ej_titles_json ej_id ej_title ej_n_remain
        ej_n_remain="${#members[@]}"
        # shellcheck disable=SC2086
        ej_titles_json="$(bdjson show $lg_ejected 2>/dev/null)" || ej_titles_json="[]"
        for ej_id in $lg_ejected; do
            ej_title="$(printf '%s\n' "$ej_titles_json" | python3 -c "
import json, sys
data = json.load(sys.stdin)
items = data if isinstance(data, list) else [data]
t = next((str(i.get('title','')) for i in items if i.get('id') == '$ej_id'), '')
print((t[:120] if t else '(title unavailable)') or '(title unavailable)')
" 2>/dev/null)" || ej_title="(title unavailable)"
            "$forge" pr-comment "$repo" "$pr_n" \
                "Ejected: $ej_id — ${ej_title:-(title unavailable)} (${lg_suites_csv:-unknown}); $ej_n_remain remain" \
                2>/dev/null || true
        done
    fi

    # Record the open batch.
    local bdir; bdir="$(dirname "$(_batch_open_file "$name")")"
    mkdir -p "$bdir"
    {
        printf 'pr=%s\n'      "$pr_n"
        printf 'head=%s\n'    "$batch_head"
        printf 'base=%s\n'    "$base_sha"
        printf 'members=%s\n' "${members[*]}"
        printf 'opened=%s\n'  "$now"
        printf 'branch=%s\n'  "$batch_br"
    } > "$(_batch_open_file "$name")"

    # Mark each member BATCHED.
    local _mm _mid _mtip
    for _mm in "${members[@]}"; do
        _mid="${_mm%%:*}"; _mtip="${_mm##*:}"
        land_mark "$_mid" BATCHED "$_mtip"
    done
    rm -f "$SPIRA_RUN/queue-stuck-$name" 2>/dev/null || true

    printf 'batch %s: PR %s opened — %d branches (%s)\n' \
        "$name" "$pr_n" "${#members[@]}" "$batch_br"
}

main "$@"
