#!/usr/bin/env bash
#
# test-batcher.sh — the native cargo job for the batcher crate's pure core (sp-xr8rf).
#
# The core has no IO: no git, no testenv-batch, no forge, no bead store, no wall clock. Its
# replay tests are ordinary `cargo test` unit tests, so this suite is test-cockpit-rust.sh's
# shape with the fixture database dropped — there is nothing here that needs one.
#
# testlib.sh's report_cargo parses cargo test's own per-test lines and calls ok/bad once
# per Rust test, so a red names the failing case instead of folding 40-odd tests into one
# line.
#
# tier: T1
# covers: batcher/src/*
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

# RESOLVE THE TOOLCHAIN EXPLICITLY. cargo execs `rustc` BY NAME, and conf.sh overwrites PATH
# with the harness's own tool directories, which do not include ~/.cargo/bin.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin — install Rust: https://rustup.rs/"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

OUT="$TMP/cargo-test.out"
CARGO_TERM_COLOR=never "$CARGO_BIN" test --manifest-path "$ROOT/Cargo.toml" \
    -p batcher --no-fail-fast > "$OUT" 2>&1
_rc=$?
cat "$OUT"
report_cargo "$OUT" "$_rc"

tl_summary
