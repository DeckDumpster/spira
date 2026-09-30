#!/usr/bin/env bash
#
# build.sh — build the Rust programs this harness ships.
#
# Delegates to `make build` in SPIRA_REPO, which runs `cargo build --release
# --workspace`. A missing cargo is now a hard failure; install Rust first.
#
# USAGE
#   build.sh [--with-bd] [--skip-build]
#
#     --with-bd     build bd through build-bd.sh --install after the Rust programs.
#     --skip-build  skip building entirely — for a container that mounts prebuilt binaries.
#                   Exits 0 after printing the expected paths.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

with_bd=0
skip_build=0
while [ $# -gt 0 ]; do
    case "$1" in
        --with-bd)     with_bd=1;    shift ;;
        --skip-build)  skip_build=1; shift ;;
        *) printf 'build.sh: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

if [ "$skip_build" = 1 ]; then
    printf 'build.sh: --skip-build — prebuilt binaries expected on PATH (the release bin/):\n'
    for _b in loom panel broker czar-pass spira-supervise landing-pass; do
        printf '  %-14s %s\n' "$_b:" "$(command -v "$_b" || printf 'NOT ON PATH')"
    done
    exit 0
fi

make -C "$SPIRA_REPO" build || {
    printf 'build.sh: make build failed — install Rust (https://rustup.rs/) if cargo is missing\n' >&2
    exit 1
}

if [ "$with_bd" = 1 ]; then
    printf 'build.sh: building bd (--with-bd)\n'
    build-bd.sh --install
fi

printf 'build.sh: done\n'
