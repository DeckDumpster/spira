#!/usr/bin/env bash
#
# build.sh — build the Rust programs this harness ships.
#
# Loom (the read endpoint over the beads graph) and the cockpit panel (the attention surface)
# are both cargo projects. Nothing in install.sh or elsewhere builds them; a clone that skips
# this script gets a spira-loom.service that exits 2 on every start and an empty attention
# pane.
#
# USAGE
#   build.sh [--with-bd] [--skip-build]
#
#     --with-bd     build bd through build-bd.sh --install after the Rust programs are built.
#                   Requires go and gcc on PATH; see build-bd.sh.
#     --skip-build  skip building entirely — for a container that mounts prebuilt binaries.
#                   Exits 0 after printing the expected paths.
#
# CARGO ABSENT IS A WARNING, NOT A FAILURE. The loop runs without the panel: a machine with
# no cargo installed can still run Spira. When cargo is not found, build.sh names what will
# not work and exits 0 so the rest of the install proceeds.
#
# IDEMPOTENT BY CONSTRUCTION. cargo build --release is already incremental: a second run on
# an unchanged tree spends a fraction of a second and produces no output. Running build.sh
# twice is safe and cheap.
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

# Source directories derived from SPIRA_REPO and SPIRA_COCKPIT, which conf.sh computed
# from where this file sits — no absolute path leaves this file.
LOOM_DIR="$SPIRA_REPO/loom"
PANEL_DIR="$SPIRA_COCKPIT/panel"
BROKER_DIR="$SPIRA_REPO/broker"
CZAR_PASS_DIR="$SPIRA_REPO/czar-pass"

if [ "$skip_build" = 1 ]; then
    printf 'build.sh: --skip-build — prebuilt binaries expected at:\n'
    printf '  loom:      %s\n' "$SPIRA_LOOM_BIN"
    printf '  panel:     %s\n' "$SPIRA_PANEL"
    printf '  broker:    %s\n' "$SPIRA_BROKER_BIN"
    printf '  czar-pass: %s\n' "$SPIRA_CZAR_PASS_BIN"
    exit 0
fi

# A MISSING CARGO IS A WARNING. The panel is the feature lost; the loop and every other part
# of the harness keeps running. Name exactly what will not work so the operator knows why the
# attention pane is empty and how to fix it.
if ! command -v cargo >/dev/null 2>&1; then
    printf 'build.sh: cargo not on PATH — loom, the cockpit panel and the broker will not be built\n' >&2
    printf 'build.sh:   features lost: the Loom read endpoint, the attention panel, and the forge-write broker\n' >&2
    printf 'build.sh:   to fix: install Rust (https://rustup.rs/) and re-run build.sh\n' >&2
    printf 'build.sh:   PATH is %s\n' "$PATH" >&2
    printf 'build.sh: the harness loop continues without them\n' >&2
    # bd is still built if requested — it uses go, not cargo.
    if [ "$with_bd" = 1 ]; then
        printf 'build.sh: building bd (cargo absent does not affect this step)\n'
        bash "$HERE/build-bd.sh" --install
    fi
    exit 0
fi

# LOOM — the read endpoint over the beads graph. Built first because it gates the Loom
# service; the panel is cosmetic by comparison.
if [ ! -d "$LOOM_DIR" ]; then
    printf 'build.sh: loom source directory not found at %s\n' "$LOOM_DIR" >&2
    exit 1
fi
printf 'build.sh: building loom (release)\n'
( cd "$LOOM_DIR" && cargo build --release ) || {
    printf 'build.sh: loom build failed\n' >&2; exit 1; }
printf 'build.sh: loom built at %s\n' "$SPIRA_LOOM_BIN"

# PANEL — the cockpit attention surface. Built second; its absence does not prevent the loop
# from running, but doctor.sh warns when it is missing.
if [ ! -d "$PANEL_DIR" ]; then
    printf 'build.sh: panel source directory not found at %s\n' "$PANEL_DIR" >&2
    exit 1
fi
printf 'build.sh: building panel (release)\n'
( cd "$PANEL_DIR" && cargo build --release ) || {
    printf 'build.sh: panel build failed\n' >&2; exit 1; }
printf 'build.sh: panel built at %s\n' "$SPIRA_PANEL"

# BROKER — the forge-write broker. Aeons submit intents; this credentialed executor validates
# and runs them. Built third; its absence disables forge writes from aeons but does not stop
# the loop.
if [ ! -d "$BROKER_DIR" ]; then
    printf 'build.sh: broker source directory not found at %s\n' "$BROKER_DIR" >&2
    exit 1
fi
printf 'build.sh: building broker (release)\n'
( cd "$BROKER_DIR" && cargo build --release ) || {
    printf 'build.sh: broker build failed\n' >&2; exit 1; }
printf 'build.sh: broker built at %s\n' "$SPIRA_BROKER_BIN"

# CZAR-PASS — the czar fast-pass binary. Queue-stall detection on a 30-second timer.
if [ ! -d "$CZAR_PASS_DIR" ]; then
    printf 'build.sh: czar-pass source directory not found at %s\n' "$CZAR_PASS_DIR" >&2
    exit 1
fi
printf 'build.sh: building czar-pass (release)\n'
( cd "$CZAR_PASS_DIR" && cargo build --release ) || {
    printf 'build.sh: czar-pass build failed\n' >&2; exit 1; }
printf 'build.sh: czar-pass built at %s\n' "$SPIRA_CZAR_PASS_BIN"

# BD — optional, only when --with-bd was given.
if [ "$with_bd" = 1 ]; then
    printf 'build.sh: building bd (--with-bd)\n'
    bash "$HERE/build-bd.sh" --install
fi

printf 'build.sh: done\n'
