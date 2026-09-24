#!/usr/bin/env bash
#
# build-tarball.sh — build a Spira release tarball from a source checkout.
#
# USAGE
#   build-tarball.sh build [--output <dir>] [--workspace <path>] [<commit> [<repo>]]
#   build-tarball.sh verify <release-dir> --repo <git-repo>
#
#   The 'build' subcommand is the default: omitting it is equivalent.
#   --workspace <path>: auto-discover all [[bin]] targets via cargo metadata.
#   Legacy: --loom-bin, --panel-bin, --broker-bin, --supervise-bin still accepted.
#
# TARBALL CONTENTS
#   Every file tracked by git at the given commit, plus:
#     bin/<name>  — one entry per workspace [[bin]] target (or per explicit --*-bin)
#     MANIFEST    — commit sha, timestamp, and sha256 per binary
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
#       --panel-bin cockpit/panel/target/release/panel \
#       --broker-bin broker/target/release/broker \
#       --supervise-bin supervise/target/release/spira-supervise
#
#   # Verify a release directory:
#   build-tarball.sh verify /opt/spira-releases/current --repo /path/to/harness
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# ---------------------------------------------------------------------------
# build — produce the tarball
# ---------------------------------------------------------------------------
do_build() {
    local outdir="." loom_bin="" panel_bin="" broker_bin="" supervise_bin="" commit="" repo="" name_override=""
    local workspace=""  # workspace root for auto-discovery via cargo metadata

    while [ $# -gt 0 ]; do
        case "$1" in
            --output)        outdir="$2";        shift 2 ;;
            --loom-bin)      loom_bin="$2";      shift 2 ;;
            --panel-bin)     panel_bin="$2";     shift 2 ;;
            --broker-bin)    broker_bin="$2";    shift 2 ;;
            --supervise-bin) supervise_bin="$2"; shift 2 ;;
            --workspace)     workspace="$2";     shift 2 ;;
            --name)          name_override="$2"; shift 2 ;;
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
        if [ -n "$workspace" ]; then
            repo="$workspace"
        else
            repo="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)" \
                || repo="$(cd "$HERE/.." && pwd -P)"
        fi
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

    # Binary resolution: --workspace auto-discovers from cargo metadata;
    # explicit --*-bin flags are the legacy path kept for backwards compat.
    local -a _bin_names=()
    local -a _bin_paths=()

    if [ -n "$workspace" ]; then
        command -v cargo >/dev/null 2>&1 || {
            printf 'build-tarball.sh: cargo not on PATH (required for --workspace)\n' >&2; exit 1; }
        local _meta
        _meta="$(cargo metadata --format-version=1 --no-deps \
            --manifest-path "$workspace/Cargo.toml" 2>/dev/null)" || {
            printf 'build-tarball.sh: cargo metadata failed for %s/Cargo.toml\n' "$workspace" >&2; exit 1; }
        local _binname
        while IFS= read -r _binname; do
            [ -n "$_binname" ] || continue
            local _binpath="$workspace/target/release/$_binname"
            if [ ! -f "$_binpath" ]; then
                printf 'build-tarball.sh: binary not built: %s\n' "$_binpath" >&2
                printf 'build-tarball.sh:   run: make build\n' >&2
                exit 1
            fi
            _bin_names+=("$_binname")
            _bin_paths+=("$_binpath")
        done < <(printf '%s' "$_meta" | python3 -c "
import json, sys
meta = json.load(sys.stdin)
for pkg in meta['packages']:
    for t in pkg['targets']:
        if 'bin' in t['kind']:
            print(t['name'])
" | sort)
        if [ "${#_bin_names[@]}" -eq 0 ]; then
            printf 'build-tarball.sh: no binary targets found in workspace %s\n' "$workspace" >&2
            exit 1
        fi
    else
        # Legacy explicit flags — all four are required.
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
        if [ -z "$broker_bin" ] || [ ! -f "$broker_bin" ]; then
            printf 'build-tarball.sh: broker binary not found: %s\n' \
                "${broker_bin:-(not specified; pass --broker-bin <path>)}" >&2
            exit 1
        fi
        if [ -z "$supervise_bin" ] || [ ! -f "$supervise_bin" ]; then
            printf 'build-tarball.sh: spira-supervise binary not found: %s\n' \
                "${supervise_bin:-(not specified; pass --supervise-bin <path>)}" >&2
            exit 1
        fi
        _bin_names=(loom panel broker spira-supervise)
        _bin_paths=("$loom_bin" "$panel_bin" "$broker_bin" "$supervise_bin")
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
    local _i
    for _i in "${!_bin_names[@]}"; do
        cp "${_bin_paths[$_i]}" "$stage/bin/${_bin_names[$_i]}"
        chmod +x "$stage/bin/${_bin_names[$_i]}"
    done

    # Write MANIFEST — commit, timestamp, and sha256 per binary.
    printf 'commit %s\ntimestamp %s\n' "$sha" "$ts" > "$stage/MANIFEST"
    for _i in "${!_bin_names[@]}"; do
        local _h; _h="$(sha256sum "$stage/bin/${_bin_names[$_i]}" | awk '{print $1}')"
        printf 'bin/%s %s\n' "${_bin_names[$_i]}" "$_h" >> "$stage/MANIFEST"
    done

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
