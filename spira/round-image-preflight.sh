#!/usr/bin/env bash
#
# round-image-preflight.sh — run the round image's setup in a throwaway base-OS container.
#
#   round-image-preflight.sh [--tree DIR] [--base REF] [--head REF] [--force]
#
# Runs only when the diff base...head touches a recipe input; every other branch pays one
# git diff. The inputs are read from round-vm/src/recipe.rs, the file the image hash is
# computed from, never from a second list. A red prints the failing setup line and the last
# 40 lines of output.

# covers: spira/round-image-preflight.sh spira/round-image-preflight-guest.sh round-vm/ami-setup.sh round-vm/src/template.sh round-vm/src/recipe.rs rust-toolchain.toml flake.nix flake.lock spira/testenv/Containerfile
set -uo pipefail

TREE="$PWD" BASE="${SPIRA_GATE_BASE:-}" HEAD_REF="${SPIRA_GATE_SELECT_HEAD:-${SPIRA_GATE_BRANCH:-HEAD}}" FORCE=0
while [ $# -gt 0 ]; do
    case "$1" in
        --tree) TREE="$2"; shift 2 ;;
        --base) BASE="$2"; shift 2 ;;
        --head) HEAD_REF="$2"; shift 2 ;;
        --force) FORCE=1; shift ;;
        *) echo "round-image-preflight: unknown argument: $1" >&2; exit 2 ;;
    esac
done
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
IMAGE="${SPIRA_PREFLIGHT_BASE_IMAGE:-docker.io/library/ubuntu:26.04}"
CACHE="${SPIRA_PREFLIGHT_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/spira-round-image-preflight}"
LIMIT="${SPIRA_PREFLIGHT_TIMEOUT:-1500}"

recipe_paths() {   # recipe_paths <tree> -> repo-relative path of every recipe input
    local rs="$1/round-vm/src/recipe.rs" p
    [ -f "$rs" ] || { echo "round-image-preflight: $rs is missing; the recipe inputs cannot be read" >&2; return 1; }
    sed -n 's/.*include_str!("\([^"]*\)")).*/\1/p' "$rs" | while IFS= read -r p; do
        realpath -m --relative-to="$1" "$1/round-vm/src/$p"
    done
}

foreign_checkouts() {   # registered repos other than this one that declare runner-deps
    [ -z "${SPIRA_PREFLIGHT_FOREIGN+x}" ] || { printf '%s\n' $SPIRA_PREFLIGHT_FOREIGN; return 0; }
    (
        . "$HERE/lib.sh" >/dev/null 2>&1 || exit 0
        home="$(spira_home_repo 2>/dev/null)"
        for n in $(repo_names 2>/dev/null); do
            [ "$n" = "$home" ] && continue
            r="$(repo_root "$n" 2>/dev/null)" || continue
            [ -f "$r/deploy/runner-deps.sh" ] && printf '%s\n' "$r"
        done
    )
}

paths="$(recipe_paths "$TREE")" || exit 1
[ -n "$paths" ] || { echo "round-image-preflight: recipe.rs lists no inputs" >&2; exit 1; }

if [ "$FORCE" = 0 ]; then
    [ -n "$BASE" ] || { echo "round-image-preflight: no base given (--base or SPIRA_GATE_BASE)" >&2; exit 2; }
    touched="$(git -C "$TREE" diff --name-only "$BASE...$HEAD_REF")" \
        || { echo "round-image-preflight: cannot diff $BASE...$HEAD_REF" >&2; exit 2; }
    hit="$(printf '%s\n' "$touched" | grep -Fxf <(printf '%s\n' "$paths") || true)"
    if [ -z "$hit" ]; then
        echo "round-image-preflight: skipped; no recipe input in the diff"
        exit 0
    fi
    echo "round-image-preflight: recipe inputs touched: $(echo $hit)"
fi

command -v podman >/dev/null 2>&1 || { echo "round-image-preflight: podman is not on PATH" >&2; exit 75; }
toolchain="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$TREE/rust-toolchain.toml")"
[ -n "$toolchain" ] || { echo "round-image-preflight: no channel in rust-toolchain.toml" >&2; exit 1; }

mkdir -p "$CACHE/registry" "$CACHE/target" "$CACHE/apt" "$CACHE/toolchains" || exit 75
log="$(mktemp)"; trap 'rm -f "$log"' EXIT
mounts=(-v "$TREE:/src:ro" -v "$HERE/round-image-preflight-guest.sh:/guest.sh:ro"
        -v "$CACHE/registry:/cache/registry" -v "$CACHE/target:/cache/target"
        -v "$CACHE/apt:/var/cache/apt/archives" -v "$CACHE/toolchains:/cache/toolchains")
foreign=""; i=0
while IFS= read -r d; do
    [ -n "$d" ] || continue
    mounts+=(-v "$d:/foreign/$i:ro"); foreign="$foreign /foreign/$i"; i=$((i + 1))
done < <(foreign_checkouts)

start=$SECONDS
timeout "$LIMIT" podman run --rm --network host "${mounts[@]}" "$IMAGE" \
    bash /guest.sh "$toolchain" "${foreign# }" >"$log" 2>&1
rc=$?
wall=$((SECONDS - start))

if [ "$rc" -ne 0 ]; then
    line="$(grep -E '^\++ ' "$log" | tail -n 1)"
    echo "round-image-preflight: RED rc=$rc after ${wall}s (image $IMAGE)" >&2
    [ "$rc" -ne 124 ] || echo "round-image-preflight: killed at the ${LIMIT}s limit" >&2
    echo "round-image-preflight: failing setup line: ${line:-<none traced>}" >&2
    echo "round-image-preflight: last 40 lines:" >&2
    tail -n 40 "$log" | cut -c1-300 >&2
    exit 1
fi
echo "round-image-preflight: setup exited 0 and each foreign runner-deps passed; wall ${wall}s"
