#!/usr/bin/env bash
#
# release.sh — cut and inspect Spira release units.
#
#   release.sh cut [<name-or-path>]
#   release.sh show <tag>
#
# A release unit is an annotated git tag that names every bead id whose
# commit reached the base branch since the previous release tag for the
# same repository. The tag is simultaneously the review unit, the deploy
# unit and the revert unit: to roll back, promote.sh to the previous tag.
#
# NAMING. Tags follow the form spira-release-<reponame>-<YYYYMMDDTHHMMSSZ>.
# The repo name comes from the repo-map. If two cuts land in the same UTC
# second a counter suffix (-2, -3, ...) is appended.
#
# THE BASE REF comes from spira_landref, never assumed to be 'main' — three
# of seven repositories here use 'master'.
#
# cut  [<name-or-path>]  Tag the HEAD of the base branch of the named
#                        repository (default: the home repository). Prints
#                        the new tag name on stdout. Zero-landed case: says
#                        so on stderr and exits 0 without creating a tag.
#
# show <tag>             Print the bead ids and commits in the named release
#                        tag. The repository is inferred from the tag message.
#
# EXIT   0  success (cut: tagged or nothing to tag; show: tag resolved)
#        1  error or refused
#        2  usage
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

# ---------------------------------------------------------------------------
# cut — create a release tag whenever the land ref moved since the last tag.
# ---------------------------------------------------------------------------
do_cut() {
    # First positional arg is the repo name or path (optional); remaining are flags.
    local arg="${1:-}"; shift 2>/dev/null || true
    local pr="" branches=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --pr)         pr="${2:-}"; shift 2 ;;
            --pr=*)       pr="${1#--pr=}"; shift ;;
            --branches)   branches="${2:-}"; shift 2 ;;
            --branches=*) branches="${1#--branches=}"; shift ;;
            *) printf 'release: unknown argument: %s\n' "$1" >&2; exit 2 ;;
        esac
    done

    # Resolve repo path and name from the argument (name, path, or default).
    local name="" repo=""
    case "$arg" in
        "")  name="$(spira_home_repo)" ;;
        */*) repo="$arg" ;;
        *)   name="$arg" ;;
    esac
    if [ -z "$repo" ]; then
        repo="$(repo_root "$name")" || {
            printf 'release: cannot find checkout for repository: %s\n' "$name" >&2
            exit 1
        }
    fi
    [ -n "$name" ] || name="$(repo_name_at "$repo" 2>/dev/null)" || name="$(basename "$repo")"

    # Resolve the base ref — never assume 'main'.
    local base
    base="$(spira_landref "$repo")" || {
        printf 'release: cannot determine base ref for %s\n' "$name" >&2
        exit 1
    }
    local sha
    sha="$(git -C "$repo" rev-parse --verify "$base" 2>/dev/null)" || {
        printf 'release: cannot resolve base ref %s in %s\n' "$base" "$repo" >&2
        exit 1
    }

    # Find the most recent release tag for this repository (if any).
    local tag_prefix="spira-release-${name}-"
    local prev_tag
    prev_tag="$(git -C "$repo" tag -l "${tag_prefix}*" | sort | tail -1)"

    # Cut when the land ref moved — even without bead ids. A batch of workflow or
    # docs commits is still a released state and deserves a tag so revert has a
    # unit to point at. Zero-landed means zero COMMITS, not zero bead ids.
    local commit_range
    if [ -n "$prev_tag" ]; then
        commit_range="${prev_tag}..${base}"
    else
        commit_range="$base"
    fi
    local commit_count
    commit_count="$(git -C "$repo" rev-list --count "$commit_range" 2>/dev/null)" || commit_count=0
    if [ "$commit_count" -eq 0 ]; then
        printf 'release: no commits since %s — no tag created\n' \
            "${prev_tag:-(none)}" >&2
        exit 0
    fi

    # Collect commit subjects and extract bead ids.
    local subjects
    if [ -n "$prev_tag" ]; then
        subjects="$(git -C "$repo" log --format='%s' "${prev_tag}..${base}" 2>/dev/null)" || subjects=""
    else
        subjects="$(git -C "$repo" log --format='%s' "$base" 2>/dev/null)" || subjects=""
    fi
    local id_prefix="${SPIRA_ID_PREFIX:-sp}"
    local ids
    ids="$(printf '%s\n' "$subjects" \
        | grep -oE "${id_prefix}-[a-z0-9]+(\.[0-9]+)?" \
        | sort -u)" || ids=""

    # Generate a timestamp-based tag name; handle the rare same-second collision.
    local ts; ts="$(date -u '+%Y%m%dT%H%M%SZ')"
    local tagname="${tag_prefix}${ts}"
    local n=1
    while git -C "$repo" rev-parse --verify "refs/tags/$tagname" >/dev/null 2>&1; do
        n=$((n + 1))
        tagname="${tag_prefix}${ts}-${n}"
    done

    # Build the tag message. PR and branch lines let show reconstruct the batch.
    # Bead lines may be absent for workflow or docs commits.
    {
        printf 'spira release: %s\n' "$name"
        printf 'base: %s (%s)\n' "$base" "$sha"
        printf 'prev: %s\n' "${prev_tag:-(none)}"
        if [ -n "$pr" ]; then printf 'pr: %s\n' "$pr"; fi
        if [ -n "$branches" ]; then
            printf '%s' "$branches" | tr ',' '\n' | while IFS= read -r _b; do
                _b="${_b#"${_b%%[![:space:]]*}"}"; _b="${_b%"${_b##*[![:space:]]}"}"
                if [ -n "$_b" ]; then printf 'branch: %s\n' "$_b"; fi
            done
        fi
        printf '\n'
        if [ -n "$ids" ]; then
            printf '%s\n' "$ids" | while IFS= read -r id; do
                printf 'bead: %s\n' "$id"
            done
        fi
    } | git -C "$repo" tag -a "$tagname" "$sha" -F - || {
        printf 'release: could not create tag %s\n' "$tagname" >&2
        exit 1
    }

    printf '%s\n' "$tagname"
}

# ---------------------------------------------------------------------------
# show — resolve a release tag to its bead ids and commits.
# ---------------------------------------------------------------------------
do_show() {
    local tag="${1:-}"
    [ -n "$tag" ] || { printf 'usage: release.sh show <tag>\n' >&2; exit 2; }

    # Find which managed repository holds this tag.
    local repo="" name n
    for n in $(spira_repos); do
        local r; r="$(repo_root "$n" 2>/dev/null)" || continue
        if git -C "$r" rev-parse --verify "refs/tags/$tag" >/dev/null 2>&1; then
            repo="$r"; name="$n"; break
        fi
    done

    if [ -z "$repo" ]; then
        printf 'release: tag %s not found in any managed repository\n' "$tag" >&2
        exit 1
    fi

    # Read the tag message via 'git tag -l --format'.
    local msg
    msg="$(git -C "$repo" tag -l --format='%(contents)' "$tag" 2>/dev/null)"

    if [ -z "$msg" ]; then
        printf 'release: %s is a lightweight tag; no bead list embedded\n' "$tag" >&2
        exit 1
    fi

    # Print the full tag message body.
    printf '%s\n' "$msg"

    # Extract bead ids and the previous tag from the message.
    local bead_ids prev_tag
    bead_ids="$(printf '%s\n' "$msg" | grep '^bead: ' | sed 's/^bead: //')"
    prev_tag="$(printf '%s\n' "$msg" | grep '^prev: ' | head -1 | sed 's/^prev: //')"

    if [ -z "$bead_ids" ]; then
        printf '(no beads recorded in this tag)\n'
        return
    fi

    # Resolve the commit this tag points to.
    local tag_sha
    tag_sha="$(git -C "$repo" rev-parse "${tag}^{commit}" 2>/dev/null)" || tag_sha=""

    # Build the commit range: from the previous tag (exclusive) to this tag.
    local from_ref=""
    if [ -n "$prev_tag" ] && [ "$prev_tag" != "(none)" ]; then
        from_ref="${prev_tag}.."
    fi
    local range="${from_ref}${tag_sha:-$tag}"

    printf '\ncommits:\n'
    while IFS= read -r id; do
        [ -n "$id" ] || continue
        # Use --fixed-strings to treat the bead id as a literal, not a regex.
        local commits
        commits="$(git -C "$repo" log --fixed-strings --format='%h %s' "$range" --grep="$id" \
            2>/dev/null)"
        if [ -n "$commits" ]; then
            while IFS= read -r line; do
                printf '  %s  %s\n' "$id" "$line"
            done <<< "$commits"
        else
            printf '  %s  ?\n' "$id"
        fi
    done <<< "$bead_ids"
}

# ---------------------------------------------------------------------------
CMD="${1:-}"; shift 2>/dev/null || true
case "$CMD" in
    cut)  do_cut "$@" ;;
    show) do_show "$@" ;;
    -h|--help) sed -n '2,36p' "$0"; exit 0 ;;
    *)
        printf 'usage: release.sh cut [<name-or-path>]\n' >&2
        printf '       release.sh show <tag>\n' >&2
        exit 2
        ;;
esac
