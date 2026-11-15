#!/usr/bin/env bash
#
# round-image-preflight.sh — run the round image's setup in a throwaway base-OS container.
#
#   round-image-preflight.sh [--tree DIR] [--base REF] [--head REF] [--force] [--sysfs DIR]
#
# Runs only when the diff base...head touches a recipe input; every other branch pays one
# git diff. A diff that only edits the apt-get install line of ami-setup.sh installs that
# line alone. The inputs are read from round-vm/src/recipe.rs, the file the image hash is
# computed from, never from a second list. A red prints the failing setup line and the last
# 40 lines of output.

# covers: spira/round-image-preflight.sh spira/round-image-preflight-guest.sh round-vm/ami-setup.sh round-vm/src/template.sh round-vm/src/recipe.rs rust-toolchain.toml flake.nix flake.lock spira/testenv/Containerfile
set -uo pipefail

TREE="$PWD" BASE="${SPIRA_GATE_BASE:-}" HEAD_REF="${SPIRA_GATE_SELECT_HEAD:-${SPIRA_GATE_BRANCH:-HEAD}}" FORCE=0 SYSFS=/sys
while [ $# -gt 0 ]; do
    case "$1" in
        --tree) TREE="$2"; shift 2 ;;
        --base) BASE="$2"; shift 2 ;;
        --head) HEAD_REF="$2"; shift 2 ;;
        --force) FORCE=1; shift ;;
        --sysfs) SYSFS="$2"; shift 2 ;;
        *) echo "round-image-preflight: unknown argument: $1" >&2; exit 2 ;;
    esac
done
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
IMAGE="${SPIRA_PREFLIGHT_BASE_IMAGE:-docker.io/library/ubuntu:26.04}"
LIMIT="${SPIRA_PREFLIGHT_TIMEOUT:-1500}"

_disk_leaves() {   # _disk_leaves <sysfs-device-dir> -> "<disk> <rotational>" per backing disk; 2 when one cannot be read
    local dir="$1" slave rc=0
    if [ -d "$dir/slaves" ] && [ -n "$(ls -A "$dir/slaves" 2>/dev/null)" ]; then
        for slave in "$dir"/slaves/*; do _disk_leaves "$(readlink -f "$slave")" || rc=2; done
        return "$rc"
    fi
    [ -f "$dir/partition" ] && dir="$(dirname "$dir")"
    [ -f "$dir/queue/rotational" ] || return 2
    echo "$(basename "$dir") $(cat "$dir/queue/rotational")"
}

rotational_device() {   # rotational_device <sysfs-root> <path>: prints each rotational disk under <path>'s filesystem and returns 0; 1 when none is; 2 when a disk cannot be resolved
    local majmin leaves rc=0
    majmin="$(findmnt -n -o MAJ:MIN -T "$2" 2>/dev/null | head -n 1 | tr -d ' ')"
    [ -n "$majmin" ] && [ -e "$1/dev/block/$majmin" ] || return 2
    leaves="$(_disk_leaves "$(readlink -f "$1/dev/block/$majmin")")" || rc=2
    [ "$rc" -eq 0 ] || return 2
    printf '%s\n' "$leaves" | awk '$2 == 1 { print $1; found = 1 } END { exit !found }'
}

recipe_paths() {   # recipe_paths <tree> -> repo-relative path of every recipe input
    local rs="$1/round-vm/src/recipe.rs" p
    [ -f "$rs" ] || { echo "round-image-preflight: $rs is missing; the recipe inputs cannot be read" >&2; return 1; }
    sed -n 's/.*include_str!("\([^"]*\)")).*/\1/p' "$rs" | while IFS= read -r p; do
        realpath -m --relative-to="$1" "$1/round-vm/src/$p"
    done
}

package_list_only() {   # package_list_only <tree> <base> <head> <hit> -> 0 iff only apt-get install lines of ami-setup.sh changed
    local tree="$1" base="$2" head="$3" hit="$4" changed
    [ "$hit" = "round-vm/ami-setup.sh" ] || return 1
    changed="$(git -C "$tree" diff -U0 "$base...$head" -- round-vm/ami-setup.sh \
        | grep -E '^[-+]' | grep -vE '^(---|\+\+\+) ')" || return 1
    [ -n "$changed" ] || return 1
    ! printf '%s\n' "$changed" | grep -qvE '^[-+]apt-get install '
}

paths="$(recipe_paths "$TREE")" || exit 1
[ -n "$paths" ] || { echo "round-image-preflight: recipe.rs lists no inputs" >&2; exit 1; }
npaths="$(printf '%s\n' "$paths" | wc -l)"

hit=""
if [ "$FORCE" = 0 ]; then
    [ -n "$BASE" ] || { echo "round-image-preflight: no base given (--base or SPIRA_GATE_BASE)" >&2; exit 2; }
    touched="$(git -C "$TREE" diff --name-only "$BASE...$HEAD_REF")" \
        || { echo "round-image-preflight: cannot diff $BASE...$HEAD_REF" >&2; exit 2; }
    hit="$(printf '%s\n' "$touched" | grep -Fxf <(printf '%s\n' "$paths") || true)"
    if [ -z "$hit" ]; then
        echo "round-image-preflight: skipped; no recipe input in the diff"
        echo "fence: round-image-preflight checked $npaths recipe paths against the diff (0 touched, setup not run)"
        exit 0
    fi
    echo "round-image-preflight: recipe inputs touched: $(echo $hit)"
fi

command -v podman >/dev/null 2>&1 || { echo "round-image-preflight: podman is not on PATH" >&2; exit 75; }

ROOT="$SPIRA_PREFLIGHT_CACHE_ROOT"
case "$ROOT" in
    /*) ;;
    *) echo "round-image-preflight: SPIRA_PREFLIGHT_CACHE_ROOT must be an absolute path on a build disk (got '${ROOT}')" >&2; exit 75 ;;
esac
mkdir -p "$ROOT" || exit 75
ROOT="$(realpath "$ROOT")"
dev="$(rotational_device "$SYSFS" "$ROOT")"; rot=$?
case "$rot" in
    0) echo "round-image-preflight: SPIRA_PREFLIGHT_CACHE_ROOT resolves to $ROOT on rotational disk $(echo $dev); name a path on a build disk" >&2; exit 75 ;;
    1) ;;
    *) echo "round-image-preflight: SPIRA_PREFLIGHT_CACHE_ROOT resolves to $ROOT, whose disk cannot be read from ${SYSFS}; refusing" >&2; exit 75 ;;
esac
CACHE="$ROOT/cache"
mkdir -p "$CACHE/registry" "$CACHE/target" "$CACHE/apt" "$CACHE/toolchains" "$ROOT/storage" "$ROOT/tmp" || exit 75
export TMPDIR="$ROOT/tmp"
log="$(mktemp)"; list="$(mktemp)"; trap 'rm -f "$log" "$list"' EXIT
git -C "$TREE" ls-files -z --cached --others --exclude-standard \
    | while IFS= read -r -d '' f; do if [ -e "$TREE/$f" ] || [ -L "$TREE/$f" ]; then printf '%s\0' "$f"; fi; done > "$list" \
    || { echo "round-image-preflight: cannot list the tree's source files" >&2; exit 1; }
mounts=(-v "$TREE:/src:ro" -v "$list:/tree.list:ro" -v "$HERE/round-image-preflight-guest.sh:/guest.sh:ro"
        -v "$CACHE/registry:/cache/registry" -v "$CACHE/target:/cache/target"
        -v "$CACHE/apt:/var/cache/apt/archives" -v "$CACHE/toolchains:/cache/toolchains")

pkg_only=0
if [ "$FORCE" = 0 ] && package_list_only "$TREE" "$BASE" "$HEAD_REF" "$hit"; then
    apt_line="$(grep -E '^apt-get install ' "$TREE/round-vm/ami-setup.sh")"
    [ "$(printf '%s\n' "$apt_line" | grep -c .)" = 1 ] && pkg_only=1
fi

start=$SECONDS
if [ "$pkg_only" = 1 ]; then
    printf '%s\n' 'set -euo pipefail' 'export DEBIAN_FRONTEND=noninteractive' 'apt-get update -qq' "$apt_line" \
        | timeout "$LIMIT" podman --root "$ROOT/storage" run --rm -i --network host -v "$CACHE/apt:/var/cache/apt/archives" "$IMAGE" \
            bash -s >"$log" 2>&1
else
    toolchain="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$TREE/rust-toolchain.toml")"
    [ -n "$toolchain" ] || { echo "round-image-preflight: no channel in rust-toolchain.toml" >&2; exit 1; }
    timeout "$LIMIT" podman --root "$ROOT/storage" run --rm --network host "${mounts[@]}" "$IMAGE" \
        bash /guest.sh "$toolchain" >"$log" 2>&1
fi
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
if [ "$pkg_only" = 1 ]; then
    echo "round-image-preflight: package list installs; wall ${wall}s"
    echo "fence: round-image-preflight checked $npaths recipe paths against the diff (1 touched, package list installed)"
    exit 0
fi
echo "round-image-preflight: setup exited 0; wall ${wall}s"
echo "fence: round-image-preflight checked $npaths recipe paths against the diff ($(printf "%s" "$hit" | grep -c .) touched, setup ran)"
