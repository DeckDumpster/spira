#!/usr/bin/env bash
# held.sh — report branches waiting on a human in land=hold repositories.
#
#   held.sh [<repo>]              full table (all hold-mode repos, or named one)
#   held.sh --summary [<repo>]   one-liner per repo; silent when nothing is held
#   held.sh --merge <bead-id>    fast-forward base to that branch (confirmation required)
#   held.sh --drop-empty [<repo>] delete every branch with 0 commits ahead of base
#
# RULES ENCODED HERE
# ------------------
#   ahead    counted against spira_landref, never a hardcoded branch name
#   EMPTY    only when ahead == 0; only empties are offered for bulk removal
#   ORPHAN   branch whose bead no longer exists — more dangerous to drop, not less
#   NO REMOTE  printed unconditionally first; changes every other judgement on the page
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/lib.sh"

_usage() {
    cat >&2 <<'EOF'
held.sh [--summary | --merge <bead-id> | --drop-empty] [<repo>]

  (no args)         full held-branch table for all land=hold repos
  --summary         one-liner per repo; silent when nothing is held
  --merge <id>      fast-forward base to the named branch (asks for confirmation)
  --drop-empty      delete every branch with 0 commits ahead of base
  --help / -h       this message
EOF
    exit 1
}

MODE=table
MERGE_TARGET=""
REPO_ARG=""

while [ $# -gt 0 ]; do
    case "$1" in
        --summary)    MODE=summary ;;
        --merge)      MODE=merge; MERGE_TARGET="${2:?--merge needs a bead id}"; shift ;;
        --drop-empty) MODE=drop-empty ;;
        --help|-h)    _usage ;;
        -*)           printf 'held.sh: unknown flag %s\n' "$1" >&2; exit 2 ;;
        *)            REPO_ARG="$1" ;;
    esac
    shift
done

# Emit repo names to process: the named one, or all hold-mode repos from the map.
_hold_repos() {
    if [ -n "$REPO_ARG" ]; then
        printf '%s\n' "$REPO_ARG"
        return
    fi
    repo_names | while IFS= read -r name; do
        [ "$(repo_land "$name")" = hold ] && printf '%s\n' "$name"
    done
}

# Bead status string, or "(none)" when the bead does not exist in the store.
_bead_status() {
    bdjson show "$1" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print("(none)"); sys.exit()
d = d if isinstance(d, list) else [d]
s = d[0].get("status", "") if d else ""
print(s.upper() if s else "(none)")' 2>/dev/null
}

_ahead()  { git -C "$1" rev-list --count "${3}..$2"  2>/dev/null || printf '?'; }
_behind() { git -C "$1" rev-list --count "$2..${3}"  2>/dev/null || printf '?'; }

# _collect <repo-name> — populate branch arrays and summary counts from a hold-mode repo.
# Writes: BRANCHES BEAD_IDS STATES AHEAD_COUNTS BEHIND_COUNTS WORKTREES VERDICTS
#         _repo_path _base_ref _has_remote total_commits empty_count orphan_count held_count
_collect() {
    local name="$1"
    _repo_path="$(repo_root "$name" 2>/dev/null)" \
        || { printf 'held.sh: repo %s not in map\n' "$name" >&2; return 1; }
    _base_ref="$(spira_landref "$_repo_path" 2>/dev/null)" \
        || { printf 'held.sh: could not resolve landref for %s\n' "$name" >&2; return 1; }
    _has_remote="$(git -C "$_repo_path" remote 2>/dev/null)"

    BRANCHES=(); BEAD_IDS=(); STATES=(); AHEAD_COUNTS=(); BEHIND_COUNTS=()
    WORKTREES=(); VERDICTS=()
    total_commits=0; empty_count=0; orphan_count=0

    local branch bead_id ahead behind bead_state wt wt_display verdict
    while IFS= read -r branch; do
        [ -n "$branch" ] || continue
        bead_id="${branch#spira/}"
        ahead="$(_ahead "$_repo_path" "$branch" "$_base_ref")"
        behind="$(_behind "$_repo_path" "$branch" "$_base_ref")"
        bead_state="$(_bead_status "$bead_id")"
        wt="$(worktree_of "$branch" "$_repo_path" 2>/dev/null)"
        wt_display="${wt:+live}"; wt_display="${wt_display:--}"

        if [ "$bead_state" = "(none)" ]; then
            verdict="ORPHAN — bead is gone"
            orphan_count=$(( orphan_count + 1 ))
        elif [ "${ahead:-0}" -eq 0 ] 2>/dev/null; then
            verdict="EMPTY — safe to drop"
            empty_count=$(( empty_count + 1 ))
        else
            verdict="HELD — awaiting you"
            total_commits=$(( total_commits + ahead ))
        fi

        BRANCHES+=("$branch")
        BEAD_IDS+=("$bead_id")
        STATES+=("$bead_state")
        AHEAD_COUNTS+=("$ahead")
        BEHIND_COUNTS+=("$behind")
        WORKTREES+=("$wt_display")
        VERDICTS+=("$verdict")
    done < <(git -C "$_repo_path" for-each-ref \
        --format='%(refname:short)' 'refs/heads/spira/' 2>/dev/null | sort)

    held_count=$(( ${#BRANCHES[@]} - empty_count - orphan_count ))
}

_print_table() {
    local name="$1"
    local w_br=6 w_bead=4 w_state=10 i fmt

    printf '%s — land=hold' "$name"
    [ -z "$_has_remote" ] && printf ', NO REMOTE (no second copy of anything below)'
    printf '\n\n'

    if [ "${#BRANCHES[@]}" -eq 0 ]; then
        printf '  (no spira/* branches)\n'
        return 0
    fi

    for i in "${!BRANCHES[@]}"; do
        [ "${#BRANCHES[$i]}" -gt "$w_br" ]   && w_br="${#BRANCHES[$i]}"
        [ "${#BEAD_IDS[$i]}" -gt "$w_bead" ] && w_bead="${#BEAD_IDS[$i]}"
        [ "${#STATES[$i]}" -gt "$w_state" ]  && w_state="${#STATES[$i]}"
    done
    fmt="  %-${w_br}s  %-${w_bead}s  %-${w_state}s  %5s  %6s  %-8s  %s\n"
    # shellcheck disable=SC2059
    printf "$fmt" branch bead "bead state" ahead behind worktree verdict
    # shellcheck disable=SC2059
    printf "$fmt" ------ ---- ---------- ----- ------ -------- -------
    for i in "${!BRANCHES[@]}"; do
        # shellcheck disable=SC2059
        printf "$fmt" "${BRANCHES[$i]}" "${BEAD_IDS[$i]}" "${STATES[$i]}" \
            "${AHEAD_COUNTS[$i]}" "${BEHIND_COUNTS[$i]}" "${WORKTREES[$i]}" "${VERDICTS[$i]}"
    done

    printf '\n'
    [ "$held_count" -gt 0 ] && printf '  %d branch%s holding %d commit%s that exist nowhere else.\n' \
        "$held_count" "$([ "$held_count" -eq 1 ] || printf 'es')" \
        "$total_commits" "$([ "$total_commits" -eq 1 ] || printf 's')"
    [ "$empty_count" -gt 0 ]  && printf '  %d empty.\n'  "$empty_count"
    [ "$orphan_count" -gt 0 ] && printf '  %d orphan.\n' "$orphan_count"

    if [ "$held_count" -gt 0 ] || [ "$orphan_count" -gt 0 ]; then
        printf '\n'
        [ "$held_count" -gt 0 ]  && printf '  Merge one:    held.sh --merge <bead-id>\n'
        [ "$empty_count" -gt 0 ] && printf '  Drop empties: held.sh --drop-empty\n'
    fi
}

# ---- TABLE MODE -----------------------------------------------------------------------
if [ "$MODE" = table ]; then
    first=1
    while IFS= read -r repo_name; do
        BRANCHES=(); BEAD_IDS=(); STATES=(); AHEAD_COUNTS=(); BEHIND_COUNTS=()
        WORKTREES=(); VERDICTS=()
        _repo_path=""; _base_ref=""; _has_remote=""; total_commits=0
        empty_count=0; orphan_count=0; held_count=0

        _collect "$repo_name" || continue
        [ "$first" -eq 1 ] || printf '\n'
        first=0
        _print_table "$repo_name"
    done < <(_hold_repos)
    exit 0
fi

# ---- SUMMARY MODE ---------------------------------------------------------------------
if [ "$MODE" = summary ]; then
    while IFS= read -r repo_name; do
        BRANCHES=(); BEAD_IDS=(); STATES=(); AHEAD_COUNTS=(); BEHIND_COUNTS=()
        WORKTREES=(); VERDICTS=()
        _repo_path=""; _base_ref=""; _has_remote=""; total_commits=0
        empty_count=0; orphan_count=0; held_count=0

        _collect "$repo_name" 2>/dev/null || continue
        [ "${#BRANCHES[@]}" -eq 0 ] && continue

        s="HOLD   ${repo_name}"
        [ -z "$_has_remote" ] && s="$s · no remote"
        [ "$held_count" -gt 0 ] && \
            s="$s · $held_count branch$([ "$held_count" -eq 1 ] || printf 'es') · $total_commits commit$([ "$total_commits" -eq 1 ] || printf 's') awaiting you"
        [ "$empty_count" -gt 0 ]  && s="$s · $empty_count empty"
        [ "$orphan_count" -gt 0 ] && s="$s · $orphan_count orphan"
        printf '%s\n' "$s"
    done < <(_hold_repos)
    exit 0
fi

# ---- DROP-EMPTY MODE ------------------------------------------------------------------
if [ "$MODE" = drop-empty ]; then
    any=0
    while IFS= read -r repo_name; do
        BRANCHES=(); BEAD_IDS=(); STATES=(); AHEAD_COUNTS=(); BEHIND_COUNTS=()
        WORKTREES=(); VERDICTS=()
        _repo_path=""; _base_ref=""; _has_remote=""; total_commits=0
        empty_count=0; orphan_count=0; held_count=0

        _collect "$repo_name" || continue
        [ "$empty_count" -eq 0 ] && continue
        any=1

        to_drop=()
        for i in "${!VERDICTS[@]}"; do
            [[ "${VERDICTS[$i]}" == EMPTY* ]] && to_drop+=("${BRANCHES[$i]}")
        done

        printf 'Will delete %d empty branch(es) from %s:\n' "${#to_drop[@]}" "$repo_name"
        printf '  %s\n' "${to_drop[@]}"
        printf 'Continue? [y/N] '
        read -r ans
        [[ "$ans" =~ ^[Yy]$ ]] || { printf 'Aborted.\n'; exit 1; }

        for br in "${to_drop[@]}"; do
            SPIRA_REF_SANCTIONED=1 git -C "$_repo_path" branch -d "$br" 2>/dev/null \
                || SPIRA_REF_SANCTIONED=1 git -C "$_repo_path" branch -D "$br" \
                && printf '  deleted %s\n' "$br" \
                || printf '  FAILED to delete %s\n' "$br"
        done
    done < <(_hold_repos)
    [ "$any" -eq 0 ] && printf 'No empty branches to drop.\n'
    exit 0
fi

# ---- MERGE MODE -----------------------------------------------------------------------
if [ "$MODE" = merge ]; then
    target_branch="spira/${MERGE_TARGET}"
    found_repo=""; found_base=""; found_ahead=""; found_behind=""

    while IFS= read -r repo_name; do
        BRANCHES=(); BEAD_IDS=(); STATES=(); AHEAD_COUNTS=(); BEHIND_COUNTS=()
        WORKTREES=(); VERDICTS=()
        _repo_path=""; _base_ref=""; _has_remote=""; total_commits=0
        empty_count=0; orphan_count=0; held_count=0

        _collect "$repo_name" 2>/dev/null || continue
        for i in "${!BRANCHES[@]}"; do
            [ "${BRANCHES[$i]}" = "$target_branch" ] || continue
            found_repo="$_repo_path"
            found_base="$_base_ref"
            found_ahead="${AHEAD_COUNTS[$i]}"
            found_behind="${BEHIND_COUNTS[$i]}"
            break
        done
        [ -n "$found_repo" ] && break
    done < <(_hold_repos)

    if [ -z "$found_repo" ]; then
        printf 'held.sh: no branch %s in any hold-mode repo\n' "$target_branch" >&2
        exit 1
    fi

    if [ "${found_ahead:-0}" -eq 0 ] 2>/dev/null; then
        printf '%s has 0 commits ahead of base — nothing to merge.\n' "$target_branch" >&2
        exit 1
    fi

    local_base="$(ref_branch "$found_base")"
    printf 'Merge %s into %s?\n' "$target_branch" "$local_base"
    printf '  %s commits ahead, %s commits behind\n' "$found_ahead" "$found_behind"
    printf 'Continue? [y/N] '
    read -r ans
    [[ "$ans" =~ ^[Yy]$ ]] || { printf 'Aborted.\n'; exit 1; }

    git -C "$found_repo" checkout "$local_base" \
        && git -C "$found_repo" merge --ff-only "$target_branch" \
        && printf 'Merged %s into %s.\n' "$target_branch" "$local_base" \
        || { printf 'Merge failed.\n' >&2; exit 1; }
    exit 0
fi
