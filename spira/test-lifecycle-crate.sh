#!/usr/bin/env bash
#
# test-lifecycle-crate.sh — run the lifecycle crate's own cargo tests: the exhaustive
# (state, event) tables for all three machines, the property tests (terminal absorption,
# the tip invariant, expect-mismatch-never-mutates, version monotonicity, IN_DELIVERY left
# only by an exit event), and the replay-equals-incremental-application tests.
#
#   ./test-lifecycle-crate.sh
#
# WHERE IT RUNS: the TIMED set (found by the `spira/test-*.sh` glob, run by `suites.sh`),
# not `gate-suites` — nothing calls this crate yet (sp-uwv2s ships it inert), so nothing
# depends on it at landing time.
#
# host-reason: a pure `cargo test` run with no I/O of its own; nothing here reaches the
# network, a database or another process, so there is nothing for a container to isolate.
#
# defect: sp-uwv2s
# tier: T0
# covers: lifecycle/*
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$HERE/../lifecycle"
. "$HERE/testlib.sh"

# Resolve cargo/rustc BEFORE conf.sh can overwrite PATH with the harness's own tool
# directories, which do not include ~/.cargo/bin (see test-spira-config.sh's own note).
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin — install Rust: https://rustup.rs/"
export PATH="$(dirname "$CARGO_BIN"):$PATH"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
OUT="$TMP/cargo-test.out"
CARGO_TERM_COLOR=never "$CARGO_BIN" test --manifest-path "$CRATE/Cargo.toml" \
    --no-fail-fast > "$OUT" 2>&1
_rc=$?
cat "$OUT"
report_cargo "$OUT" "$_rc"

tl_summary
