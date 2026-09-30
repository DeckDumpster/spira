#!/usr/bin/env bash
#
# test-cockpit-rust.sh — the native cargo job for cockpit-observability's Rust crates.
#
#   ./test-cockpit-rust.sh
#
# THE FAILURE THIS SUITE EXISTS FOR. test-panel.sh and the deleted test-loom.sh each ran
# `cargo test` and reported it as ONE ok/FAIL line — 120 tests in the panel collapsed to a
# suite that claimed 97 in its own header and never noticed the drift. A red said nothing
# about which Rust test broke; a reader had to open the log and read cargo's own output by
# eye.
#
# WHAT THIS DOES INSTEAD. testlib.sh's report_cargo parses cargo test's own per-test lines
# and calls ok/bad once per Rust test, so each one gets its own TAP14 line and JSONL row
# keyed to the UC ids on the # covers: line below, instead of being folded into a single
# suite-level count.
#
# ONE JOB, NOT TWO SHIMS. Loom's tests/endpoint.rs is hermetic under a plain `cargo test` (a
# fake `bd` serving a canned answer). Panel and loom build against one cached workspace target
# (`cargo test -p panel -p loom` in a single invocation) instead of two separate `cargo test`
# processes each paying their own link time.
#
# THE REAL-BD FIXTURE IS GONE (sp-o8n10, law-a-test-that-flips-is-deleted, 2026-09-30). This
# suite used to build a throwaway Dolt fixture and run loom's one #[ignore]d contract test,
# real_bd_answers_in_the_shape_the_fake_is_built_from, with --include-ignored. That test flipped
# red under full-corpus load and green twice in isolation on the same tree the corpus ran on —
# the shared testdb/bd infrastructure under contention (sp-nmzok), not this endpoint's own
# logic. The test and this suite's fixture-building are deleted together; see
# docs/test-plan/cockpit-observability.md for the coverage gap and the bead to re-add it.
#
# tier: T2
# covers: cockpit/panel/src/* loom/src/* loom/tests/* UC-cockpit-observability-27 UC-cockpit-observability-28 UC-cockpit-observability-29 UC-cockpit-observability-31 UC-cockpit-observability-32
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

# RESOLVE THE TOOLCHAIN EXPLICITLY, AND SKIP RATHER THAN FAIL WHEN IT IS ABSENT. cargo execs
# `rustc` BY NAME, and conf.sh overwrites PATH with the harness's own tool directories, which
# do not include ~/.cargo/bin — see test-panel.sh's own note. SPIRA_PATH is the seam conf.sh
# honours for exactly this, so both are set.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin — install Rust: https://rustup.rs/"
_CARGO_DIR="$(dirname "$CARGO_BIN")"
export SPIRA_PATH="$_CARGO_DIR${SPIRA_PATH:+:$SPIRA_PATH}"
export PATH="$_CARGO_DIR:$PATH"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

OUT="$TMP/cargo-test.out"
CARGO_TERM_COLOR=never "$CARGO_BIN" test --manifest-path "$ROOT/Cargo.toml" \
    -p panel -p loom --no-fail-fast > "$OUT" 2>&1
_rc=$?
cat "$OUT"
report_cargo "$OUT" "$_rc"

tl_summary
