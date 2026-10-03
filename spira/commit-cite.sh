#!/usr/bin/env bash
#
# commit-cite.sh — every bead id cited in a commit message about to land must resolve in
# the bead store.
#
#   commit-cite.sh land <repo> <base> <branch>   the landing gate's fence
#
# WHY THIS EXISTS. A bead id in a commit message is free text: nothing checked it, so a
# commit could claim a follow-up was filed as some id and that id could simply not exist —
# invented, mistyped, or a promise never kept — and land unnoticed, because nothing read
# the claim before it did (sp-vx3oi).
#
# THIS CHECKS EXISTENCE ONLY, never status: a citation naming a closed bead, or one from
# years ago, is exactly as valid as one naming something open today. A citation is a claim
# that the id refers to real provenance, and a real bead in any state satisfies that claim.
#
# EXIT   0  no cited id is phantom — including the ordinary case where none was cited
#        1  a cited id resolves to nothing — each is named on stdout with the commit that
#           cited it
#        3  could not check — the bead store did not answer; said out loud, never read as
#           "every id exists" (law-absence-needs-a-positive-control)
set -uo pipefail
_cc_init_done=0
trap '[ "$_cc_init_done" = 0 ] && exit 3' EXIT
. "$(dirname "$0")/lib.sh"
_cc_init_done=1
trap - EXIT

# A citation ends at a non-id character: sp-ow-mail is one hyphenated token, not the id sp-ow.
BEAD_ID_RE='(?<![A-Za-z0-9_-])sp-[a-z0-9]++(\.[0-9]++)*+(?![A-Za-z0-9_]|-[A-Za-z0-9])'

# bead_ids_present <ids...> -> the subset that resolve in the store, one per line.
# Empty stdin/output from `bd show` (a store that could not be reached at all) is
# distinguished from a store that answered "none of these match" — the latter is valid
# JSON naming zero results, the former is not JSON at all. Only the first is a machinery
# fault; the second is exactly the phantom-citation case this fence exists to catch.
bead_ids_present() {
    local json
    json="$(bdq show "$@" --json 2>/dev/null)"
    python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(2)
for item in (data if isinstance(data, list) else []):
    bid = item.get("id")
    if bid:
        print(bid)
' <<<"$json"
}

cmd_land() {
    local repo="$1" base="$2" branch="$3"
    git -C "$repo" rev-parse --verify -q "$base" >/dev/null 2>&1 \
        && git -C "$repo" rev-parse --verify -q "$branch" >/dev/null 2>&1 || {
        printf 'commit-cite: %s or %s does not resolve in %s\n' "$base" "$branch" "$repo" >&2
        return 3
    }

    # cited[id]="<hash> <hash> ..." — every commit (by full hash) that names this id, so a
    # phantom id's report can point at the commit that invented it rather than just the id.
    #
    # READ VIA PROCESS SUBSTITUTION, NEVER A CAPTURED VARIABLE. `git log -z` NUL-delimits
    # records, and bash strips embedded NULs from `$(...)` — a capture-then-split here would
    # silently merge every commit's hash and body into one indistinguishable blob. Reading
    # the pipe directly, with the loop in this shell (not a `| while`, which would run it in
    # a subshell and lose every update to `cited` and `all_ids` on exit), keeps both.
    local -A cited=()
    local all_ids="" rec hash body found id
    while IFS= read -r -d '' rec; do
        hash="${rec%%$'\n'*}"
        body="${rec#*$'\n'}"
        found="$(grep -oP "$BEAD_ID_RE" <<<"$body" | sort -u)"
        [ -n "$found" ] || continue
        for id in $found; do
            cited["$id"]="${cited[$id]:-} $hash"
            all_ids="$all_ids $id"
        done
    done < <(git -C "$repo" log -z --format='%H%n%B' "$base..$branch" 2>/dev/null)

    all_ids="$(printf '%s\n' $all_ids | sort -u | tr '\n' ' ')"
    [ -n "${all_ids// /}" ] || return 0

    local present parse_rc
    # shellcheck disable=SC2086 -- $all_ids is a space-joined id list; splitting is the point
    present="$(bead_ids_present $all_ids)"; parse_rc=$?
    if [ "$parse_rc" = 2 ]; then
        printf 'commit-cite: the bead store did not return a readable answer for: %s\n' "$all_ids" >&2
        return 3
    fi

    local missing=0 hashes h
    for id in $all_ids; do
        case $'\n'"$present"$'\n' in
            *$'\n'"$id"$'\n'*) continue ;;
        esac
        missing=1
        hashes="${cited[$id]:-}"
        for h in $hashes; do
            printf '%s cites %s, which does not exist in the bead store\n' "${h:0:9}" "$id"
        done
    done
    [ "$missing" = 0 ] || return 1
    return 0
}

case "${1:-}" in
land) shift; cmd_land "$@" ;;
*) printf 'usage: commit-cite.sh land <repo> <base> <branch>\n' >&2; exit 2 ;;
esac
