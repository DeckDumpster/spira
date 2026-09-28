#!/usr/bin/env bash
#
# test-supervise.sh — run spira-supervise's own cargo tests: snapshot-age boundary cases
# for the heartbeat gate (a hung collector's snapshot must stop the WATCHDOG=1 pings).
#
# WHERE IT RUNS: the TIMED set (found by the `spira/test-*.sh` glob, run by `suites.sh`),
# not `gate-suites` — nothing depended on these tests running anywhere before this suite (sp-es702).
#
# host-reason: a pure `cargo test` run with no I/O of its own beyond scratch files under
# std::env::temp_dir(); nothing here reaches the network, a database or another process.
#
# defect: sp-es702
# tier: T0
# covers: supervise/src/main.rs
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin — install Rust: https://rustup.rs/"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
OUT="$TMP/cargo-test.out"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$TMP/supervise-test-target" \
    "$CARGO_BIN" test --locked -p spira-supervise --manifest-path "$HERE/../Cargo.toml" \
    --no-fail-fast > "$OUT" 2>&1
_rc=$?
cat "$OUT"
report_cargo "$OUT" "$_rc"

tl_summary
