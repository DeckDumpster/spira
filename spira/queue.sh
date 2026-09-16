#!/usr/bin/env bash
# queue.sh — submit a branch into the merge queue.
#
#   queue.sh submit <branch>
#
# Certifies any branch by running the repository's gate. In a queue-mode
# repository, a green branch is recorded CERTIFIED for the batch builder.
# In push mode the branch is fast-forward merged to the base; in pr or
# hold mode it is certified and left for landing.sh. A branch with no
# associated bead may be submitted.
#
# covers: spira/queue.sh spira/suites.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/lib.sh"

LANDSTATE="${SPIRA_RUN}/landstate"

_q_land_mark() {  # _q_land_mark <id> <state> <tip>
    local id="$1" state="$2" tip="${3:-none}"
    mkdir -p "$LANDSTATE/$(dirname "$id")" 2>/dev/null || true
    printf '%s %s %s\n' "$state" "$tip" "$(date +%s)" \
        > "$LANDSTATE/$id.$$" 2>/dev/null \
        && mv -f "$LANDSTATE/$id.$$" "$LANDSTATE/$id" 2>/dev/null
}

cmd_submit() {
    local br="${1:-}"
    [ -n "$br" ] || { printf 'queue.sh submit: branch name required\n' >&2; return 2; }

    local repo name mode
    repo="$(repo_root)" 2>/dev/null || repo="$SPIRA_REPO"
    name="$(spira_home_repo)"
    mode="$(repo_land "$name")"

    git -C "$repo" show-ref --verify --quiet "refs/heads/$br" 2>/dev/null || {
        printf 'queue.sh submit: branch not found: %s\n' "$br" >&2
        return 1
    }

    local tip id
    tip="$(git -C "$repo" rev-parse "$br" 2>/dev/null)"
    id="${br#spira/}"

    # Certify: run the gate.
    local gate_out gate_rc
    gate_out="$(SPIRA_GATE_BEAD="$id" "$HERE/gate.sh" "$br" "$name" 2>&1)"
    gate_rc=$?

    local gate_outcome
    gate_outcome="$(spira_gate_outcome "$gate_rc")"

    if [ "$gate_rc" -ne 0 ]; then
        printf 'queue.sh submit: %s failed the gate (%s)\n' "$br" "$gate_outcome" >&2
        printf '%s\n' "$gate_out" >&2
        return 1
    fi

    case "$mode" in
    queue)
        _q_land_mark "$id" CERTIFIED "$tip"
        mkdir -p "$SPIRA_QUEUE_DIR" 2>/dev/null
        printf 'CERTIFIED %s %s\n' "$tip" "$(date +%s)" > "$SPIRA_QUEUE_DIR/$id"
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
        git -C "$repo" push -q "${base_remote:-origin}" "$br:$base_branch" 2>/dev/null || {
            printf 'queue.sh submit: push of %s to %s failed\n' "$br" "$base_branch" >&2
            return 1
        }
        _q_land_mark "$id" LANDED "$tip"
        printf 'queue.sh submit: landed %s (push)\n' "$br"
        ;;
    pr|hold)
        _q_land_mark "$id" CERTIFIED "$tip"
        printf 'queue.sh submit: certified %s (%s)\n' "$br" "$mode"
        ;;
    esac
}

case "${1:-}" in
    submit) shift; cmd_submit "$@" ;;
    *) printf 'usage: queue.sh submit <branch>\n' >&2; exit 2 ;;
esac
