#!/usr/bin/env bash
#
# build-tarball.sh — build a Spira release tarball from a source checkout.
#
# USAGE
#   build-tarball.sh build [--output <dir>] [--loom-bin <path>]
#                          [--panel-bin <path>] [<commit> [<repo>]]
#   build-tarball.sh verify <release-dir> --repo <git-repo>
#
#   The 'build' subcommand is the default: omitting it is equivalent.
#
# TARBALL CONTENTS
#   Every file tracked by git at the given commit, plus:
#     bin/loom   — prebuilt linux-x86_64 binary (required via --loom-bin)
#     bin/panel  — prebuilt linux-x86_64 binary (required via --panel-bin)
#     MANIFEST   — one line: "commit <40-hex-sha>", one line: "timestamp <ts>"
#
#   Scratch files (sp-*, *.fixed) at the repo root are not present once
#   sp-tlv7 lands. The builder does not exclude them — they are deleted, not
#   filtered, so the tarball is a faithful archive of the committed tree.
#
# NAMING
#   spira-<YYYYMMDDTHHMMSSZ>.tar.gz, using the same timestamp format that
#   release.sh uses for its tags. Tag and artifact name each other; there is
#   no second scheme to drift. The tarball unpacks to a directory of the same
#   name, matching the layout activate.sh expects.
#
# verify <release-dir> --repo <git-repo>
#   Reads MANIFEST from <release-dir>/MANIFEST, extracts the commit SHA, and
#   resolves it in <git-repo>. Exits 0 if the commit exists, 1 if it does not
#   or if the MANIFEST is absent or malformed.
#   POSITIVE CONTROL: a MANIFEST whose commit does not exist in the repo exits 1.
#
# EXIT
#   0   success
#   1   error
#   2   usage
#
# EXAMPLES
#   # Build from HEAD of current repo with prebuilt binaries:
#   build-tarball.sh build \
#       --loom-bin loom/target/release/loom \
#       --panel-bin cockpit/panel/target/release/panel
#
#   # Verify a release directory:
#   build-tarball.sh verify /opt/spira-releases/current --repo /path/to/harness
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# ---------------------------------------------------------------------------
# build — produce the tarball
# ---------------------------------------------------------------------------
do_build() {
    local outdir="." loom_bin="" panel_bin="" commit="" repo="" name_override=""

    while [ $# -gt 0 ]; do
        case "$1" in
            --output)    outdir="$2";        shift 2 ;;
            --loom-bin)  loom_bin="$2";      shift 2 ;;
            --panel-bin) panel_bin="$2";     shift 2 ;;
            --name)      name_override="$2"; shift 2 ;;
            -h|--help)   _usage; exit 0 ;;
            -*) printf 'build-tarball.sh: unknown option: %s\n' "$1" >&2; exit 2 ;;
            *)
                if   [ -z "$commit" ]; then commit="$1"
                elif [ -z "$repo"   ]; then repo="$1"
                else printf 'build-tarball.sh: unexpected argument: %s\n' "$1" >&2; exit 2
                fi
                shift ;;
        esac
    done

    # Default repo: the git checkout this script lives in.
    if [ -z "$repo" ]; then
        repo="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)" \
            || repo="$(cd "$HERE/.." && pwd -P)"
    fi

    # Resolve commit.
    local sha
    if [ -n "$commit" ]; then
        sha="$(git -C "$repo" rev-parse --verify "$commit^{commit}" 2>/dev/null)" || {
            printf 'build-tarball.sh: cannot resolve commit: %s\n' "$commit" >&2; exit 1; }
    else
        sha="$(git -C "$repo" rev-parse HEAD 2>/dev/null)" || {
            printf 'build-tarball.sh: cannot resolve HEAD in %s\n' "$repo" >&2; exit 1; }
    fi

    # Validate binary paths: both are required.
    if [ -z "$loom_bin" ] || [ ! -f "$loom_bin" ]; then
        printf 'build-tarball.sh: loom binary not found: %s\n' \
            "${loom_bin:-(not specified; pass --loom-bin <path>)}" >&2
        exit 1
    fi
    if [ -z "$panel_bin" ] || [ ! -f "$panel_bin" ]; then
        printf 'build-tarball.sh: panel binary not found: %s\n' \
            "${panel_bin:-(not specified; pass --panel-bin <path>)}" >&2
        exit 1
    fi

    # Generate name and timestamp. --name overrides auto-generation and pins the
    # tarball stem to match the release tag, eliminating the stamp skew that
    # occurs when the CI cuts the tag and builds the tarball in separate steps.
    local ts; ts="$(date -u '+%Y%m%dT%H%M%SZ')"
    local name
    if [ -n "$name_override" ]; then
        name="$name_override"
        local _name_ts="${name_override#spira-}"
        [[ "$_name_ts" =~ ^[0-9]{8}T[0-9]{6}Z(-[0-9]+)?$ ]] && ts="$_name_ts"
    else
        name="spira-${ts}"
    fi

    mkdir -p "$outdir"
    local outfile; outfile="$(cd "$outdir" && pwd -P)/${name}.tar.gz"

    # Stage in a temp directory so the tarball has a top-level prefix directory.
    local tmp; tmp="$(mktemp -d)"
    # Clean temp dir on any exit — placed before the first write so it fires even
    # if git archive fails.
    trap "rm -rf '$tmp'" EXIT INT TERM

    local stage="$tmp/$name"
    mkdir -p "$stage/bin"

    # Extract all tracked files at this commit.
    git -C "$repo" archive "$sha" | tar -x -C "$stage"

    # Add prebuilt binaries under bin/.
    cp "$loom_bin"  "$stage/bin/loom"
    cp "$panel_bin" "$stage/bin/panel"
    chmod +x "$stage/bin/loom" "$stage/bin/panel"

    # Write MANIFEST — the source of truth for which commit this came from.
    printf 'commit %s\ntimestamp %s\n' "$sha" "$ts" > "$stage/MANIFEST"

    # Pack. -C to the parent so the top-level entry is the versioned directory.
    tar -czf "$outfile" -C "$tmp" "$name"

    printf '%s\n' "$outfile"
}

# ---------------------------------------------------------------------------
# verify — assert that a release directory's MANIFEST names a real commit
# ---------------------------------------------------------------------------
do_verify() {
    local reldir="" repo=""

    while [ $# -gt 0 ]; do
        case "$1" in
            --repo) repo="$2"; shift 2 ;;
            -h|--help) _usage; exit 0 ;;
            -*) printf 'build-tarball.sh verify: unknown option: %s\n' "$1" >&2; exit 2 ;;
            *)
                [ -z "$reldir" ] || {
                    printf 'build-tarball.sh verify: unexpected argument: %s\n' "$1" >&2; exit 2; }
                reldir="$1"; shift ;;
        esac
    done

    [ -n "$reldir" ] || {
        printf 'usage: build-tarball.sh verify <release-dir> --repo <git-repo>\n' >&2; exit 2; }

    # Default repo: the checkout this script lives in.
    if [ -z "$repo" ]; then
        repo="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)" \
            || repo="$(cd "$HERE/.." && pwd -P)"
    fi

    local manifest="$reldir/MANIFEST"
    if [ ! -f "$manifest" ]; then
        printf 'build-tarball.sh verify: MANIFEST not found at %s\n' "$manifest" >&2
        exit 1
    fi

    # Extract the commit SHA from the first 'commit <sha>' line.
    local sha
    sha="$(grep '^commit ' "$manifest" | head -1 | awk '{print $2}')"

    # A valid commit SHA is exactly 40 lowercase hex characters.
    if [ -z "$sha" ] || ! printf '%s' "$sha" | grep -qE '^[0-9a-f]{40}$'; then
        printf 'build-tarball.sh verify: MANIFEST has no valid commit SHA (got: %s)\n' \
            "${sha:-(empty)}" >&2
        exit 1
    fi

    # Resolve the commit in the source repository.
    if ! git -C "$repo" rev-parse --verify "${sha}^{commit}" >/dev/null 2>&1; then
        printf 'build-tarball.sh verify: commit %s does not exist in %s\n' "$sha" "$repo" >&2
        exit 1
    fi

    printf 'build-tarball.sh verify: ok — commit %s\n' "$sha"
}

# ---------------------------------------------------------------------------
_usage() {
    sed -n '2,40p' "$0"
}

# ---------------------------------------------------------------------------
CMD="${1:-build}"; shift 2>/dev/null || true
case "$CMD" in
    build)    do_build "$@" ;;
    verify)   do_verify "$@" ;;
    -h|--help) _usage; exit 0 ;;
    # If CMD looks like an option or a path, treat it as a first arg to build.
    --*|-*|/*|./*|../*) do_build "$CMD" "$@" ;;
    # An unrecognised word may be a <commit> passed without the 'build' prefix.
    *) do_build "$CMD" "$@" ;;
esac
