#!/usr/bin/env bash
#
# bead.sh — file a bead through the contract; never call bd create directly.
#
#   bead.sh file "<title>" --for <persona> --repo <name> [--priority N] [--body-file F]
#   bead.sh lint [--all|<id>...]     check that beads in the store satisfy the contract
#   bead.sh contract                 legal personas, repos and kinds, read from source
#
# WHY THIS EXISTS AND NOT bd create DIRECTLY
# ------------------------------------------
# The partition labels a bead carries come from the persona's own predicate; writing them by
# hand means they can disagree, and a bead with wrong labels is either invisible (the persona
# cannot find it) or misrouted (a different persona claims it). `--for <persona>` reads the
# label set from the fayth file, so the filing tool and the claim predicate cannot disagree.
#
# `bead.sh contract` asks the live source — fayths for personas, schema.sh for kinds, the
# repo-map for repos — so the set it prints is exactly the set it can accept.
set -uo pipefail
BEAD_HOME="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$BEAD_HOME/lib.sh"

_bead_file() {
    local title="${1:-}"; shift || true
    [ -n "$title" ] || { printf 'bead: title required\n' >&2; return 2; }
    local for_fayth="" repo="" priority="" body_file=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --for)          shift; for_fayth="${1:-}" ;;
            --repo)         shift; repo="${1:-}" ;;
            --priority|-p)  shift; priority="${1:-}" ;;
            --body-file)    shift; body_file="${1:-}" ;;
            *) printf 'bead: unknown option: %s\n' "$1" >&2; return 2 ;;
        esac
        shift
    done
    [ -n "$for_fayth" ] || { printf 'bead: --for <persona> required\n' >&2; return 2; }
    [ -n "$repo" ]      || { printf 'bead: --repo <name> required\n' >&2; return 2; }
    local fpath="$SPIRA_HOME/chamber/$for_fayth.fayth"
    [ -f "$fpath" ] || { printf 'bead: no such persona: %s\n' "$for_fayth" >&2; return 2; }
    local labels; labels="$(fayth_get "$for_fayth" FAYTH_LABELS "")"
    [ -n "$labels" ] || { printf 'bead: persona %s has no partition labels\n' "$for_fayth" >&2; return 2; }
    labels="$labels,repo:$repo"
    set -- create "$title" -l "$labels"
    [ -n "$priority" ]  && set -- "$@" -p "$priority"
    [ -n "$body_file" ] && set -- "$@" --body-file "$body_file"
    bdq "$@"
}

_bead_contract() {
    printf 'PERSONAS\n'
    local f labels
    for f in $(fayth_names 2>/dev/null); do
        labels="$(fayth_get "$f" FAYTH_LABELS '' 2>/dev/null)"
        printf '  %-14s %s\n' "$f" "${labels:-(no labels)}"
    done
    printf '\nKINDS\n'
    "$BEAD_HOME/schema.sh" kinds
    printf '\nREPOS\n'
    if [ -f "${SPIRA_REPO_MAP:-}" ]; then
        grep -v '^[[:space:]]*#' "$SPIRA_REPO_MAP" | grep -v '^[[:space:]]*$' \
            | awk '{print "  " $1}'
    else
        printf '  (no repo-map)\n'
    fi
}

_bead_lint() {
    local rc=0 n=0 bad=0 id labels
    local ids=""
    if [ "${1:-}" = "--all" ] || [ $# -eq 0 ]; then
        ids="$(bdq list 2>/dev/null | awk 'NR>1{print $1}' | grep -v '^$' || true)"
    else
        ids="$*"
    fi
    for id in $ids; do
        [ -z "$id" ] && continue
        n=$((n+1))
        labels="$(bdq get "$id" 2>/dev/null | grep '^labels:' | sed 's/^labels://' || true)"
        case ",$labels," in
            *,repo:*,*) ;;
            *) printf 'bead: %s: no repo: label\n' "$id" >&2; bad=$((bad+1)); rc=1 ;;
        esac
    done
    [ "$bad" = 0 ] && printf 'bead: %d bead(s) checked, ok\n' "$n"
    return "$rc"
}

case "${1:-}" in
    file)     shift; _bead_file "$@" ;;
    contract) _bead_contract ;;
    lint)     shift; _bead_lint "$@" ;;
    *) printf 'usage: bead.sh file "<title>" --for <persona> --repo <name> [--priority N] [--body-file F]\n' >&2
       printf '       bead.sh lint [--all|<id>...]\n' >&2
       printf '       bead.sh contract\n' >&2
       exit 2 ;;
esac
