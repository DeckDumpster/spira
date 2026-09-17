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

LANDSTATE="$SPIRA_RUN/landstate"

land_state() {           # land_state <id> -> "<state> <tip> <at>" or empty
    local f="$LANDSTATE/$1"
    [ -r "$f" ] || return 1
    tr -d '\n' < "$f" 2>/dev/null
}
land_mark() {            # land_mark <id> <state> <tip> [reason]
    mkdir -p "$LANDSTATE" 2>/dev/null || return 0
    printf '%s %s %s %s' "$2" "${3:-none}" "$(date +%s)" "${4:-}" \
        > "$LANDSTATE/$1.$$" 2>/dev/null \
        && mv -f "$LANDSTATE/$1.$$" "$LANDSTATE/$1" 2>/dev/null
}

_batch_open_file() { printf '%s/%s/open' "${SPIRA_QUEUE_DIR:?}" "$1"; }

_batch_is_open() { [ -f "$(_batch_open_file "$1")" ]; }

# _certified_list <repo-path> — print "<id> <tip> <epoch>" for each CERTIFIED branch
_certified_list() {
    local br id f st tip epoch
    git -C "$1" for-each-ref --format='%(refname:short) %(objectname)' 'refs/heads/spira/*' \
        2>/dev/null \
    | while read -r br _; do
        id="${br#spira/}"
        f="$LANDSTATE/$id"
        [ -f "$f" ] || continue
        { read -r st tip epoch _ < "$f"; } 2>/dev/null || continue
        [ "$st" = "CERTIFIED" ] || continue
        printf '%s %s %s\n' "$id" "$tip" "$epoch"
    done
}

# _is_suite_transition <repo> <tip> <base-sha> — 0 if branch modifies SPIRA_SUITE_STATE
_is_suite_transition() {
    git -C "$1" diff --name-only "$3" "$2" 2>/dev/null \
        | grep -qF "${SPIRA_SUITE_STATE:-spira/suite-state}"
}

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

    base="$(spira_landref "$repo")" \
        || { printf 'batch %s: cannot resolve base ref\n' "$name" >&2; return 1; }
    base_sha="$(git -C "$repo" rev-parse "$base" 2>/dev/null)" \
        || { printf 'batch %s: cannot resolve %s\n' "$name" "$base" >&2; return 1; }
    remote="$(ref_remote "$base")"
    base_branch="$(ref_branch "$base")"

    if _batch_is_open "$name"; then
        printf 'batch %s: open batch exists — skipping\n' "$name"
        return 0
    fi

    local certs
    certs="$(_certified_list "$repo")"
    if [ -z "${certs:-}" ]; then
        rm -f "$SPIRA_RUN/queue-stuck-$name" 2>/dev/null || true
        return 0
    fi

    local count now oldest_epoch age triggered=
    count="$(printf '%s\n' "$certs" | grep -c .)"
    now="$(date +%s)"
    oldest_epoch="$(printf '%s\n' "$certs" | awk '{print $3}' | sort -n | head -1)"
    age=$(( now - oldest_epoch ))

    local _stuck_flag="$SPIRA_RUN/queue-stuck-$name"
    if [ "$age" -ge "${SPIRA_QUEUE_STUCK_AGE:-7200}" ] && [ ! -f "$_stuck_flag" ]; then
        printf '## Note\nThe oldest certified branch in %s has been waiting %ds (threshold %ds).\n\nQueue depth: %d branch(es). This may indicate a conflict loop or a stalled batch builder.\n' \
            "$name" "$age" "${SPIRA_QUEUE_STUCK_AGE:-7200}" "$count" \
        | bash "$HERE/mail.sh" send operator \
            --from "Spira Queue <queue@spira>" \
            --subject "Merge queue: $name queue stuck (${age}s)" \
            2>/dev/null && touch "$_stuck_flag" 2>/dev/null || true
        [ -f "$_stuck_flag" ] && \
            printf 'batch %s: mailed operator about stuck queue (age %ds)\n' "$name" "$age"
    fi

    [ "$count" -ge "${SPIRA_QUEUE_BATCH_MAX:-8}" ] && triggered=1
    [ "$age"   -ge "${SPIRA_QUEUE_BATCH_WAIT:-1800}" ] && triggered=1
    [ -n "$triggered" ] || return 0

    # Collect IDs for a bulk priority query.
    local all_ids=()
    while read -r _id _ _; do all_ids+=("$_id"); done <<< "$certs"
    local prio_json
    prio_json="$(bdjson show "${all_ids[@]}" 2>/dev/null)" || prio_json="[]"

    _prio_of() {
        local _pid="$1"
        printf '%s\n' "$prio_json" | python3 -c "
import sys, json
try: d = json.load(sys.stdin)
except: d = []
d = d if isinstance(d, list) else [d]
r = [x for x in d if x.get('id') == '$_pid']
print(r[0].get('priority', 9) if r else 9)" 2>/dev/null || printf '9'
    }

    # Sort: transition flag (0=trans), priority (asc), epoch (asc).
    local sortfile; sortfile="$(mktemp)"
    # shellcheck disable=SC2064
    trap "rm -f '$sortfile'" RETURN

    local _sid _stip _sepoch _is_trans _prio
    while read -r _sid _stip _sepoch; do
        _is_trans=0
        _is_suite_transition "$repo" "$_stip" "$base_sha" && _is_trans=1 || true
        _prio="$(_prio_of "$_sid")"
        printf '%d %09d %010d %s %s\n' \
            "$((1 - _is_trans))" "$_prio" "$_sepoch" "$_sid" "$_stip"
    done <<< "$certs" | sort -n > "$sortfile"

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
        if git -C "$wt" merge --no-edit --no-ff -m "spira: land $_bid" "$_btip" \
               >/dev/null 2>&1; then
            members+=("$_bid:$_btip")
            member_ids+=("$_bid")
            taken=$(( taken + 1 ))
        else
            git -C "$wt" merge --abort 2>/dev/null || true
            if _base_conflict "$repo" "$base_sha" "$_btip"; then
                # Conflict with the land ref itself — reopen the bead.
                bead_reopen "$_bid" \
                    "Reopened by batch builder: branch spira/$_bid conflicts with $base in $name." \
                    >/dev/null 2>&1 || true
                land_mark "$_bid" RED "$_btip" conflicts-with-base
                printf 'batch %s: %s conflicts with %s — reopened\n' "$name" "$_bid" "$base"
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
    batch_head="$(git -C "$wt" rev-parse HEAD 2>/dev/null)"

    if ! git -C "$repo" push -q "$remote" \
           "${batch_head}:refs/heads/${batch_br}" 2>/dev/null; then
        printf 'batch %s: could not push %s\n' "$name" "$batch_br" >&2
        return 1
    fi
    git -C "$repo" branch -f "$batch_br" "$batch_head" 2>/dev/null || true

    # Open PR via forge seam.
    local forge="${SPIRA_FORGE:-$HERE/forge.sh}"
    local pr_body pr_n
    pr_body="$(
        printf 'queue: %d branch(es)\n\n' "${#members[@]}"
        for mid in "${member_ids[@]}"; do printf -- '- %s\n' "$mid"; done
    )"
    pr_n="$(printf '%s' "$pr_body" \
            | "$forge" pr-create "$repo" "$batch_br" "$base_branch" \
                "queue: ${#members[@]} branches" 2>/dev/null)" || {
        printf 'batch %s: forge pr-create failed for %s\n' "$name" "$batch_br" >&2
        return 1
    }
    [ -n "${pr_n:-}" ] || {
        printf 'batch %s: forge returned no PR number for %s\n' "$name" "$batch_br" >&2
        return 1
    }

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
