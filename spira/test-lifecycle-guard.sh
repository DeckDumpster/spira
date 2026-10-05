#!/usr/bin/env bash
#
# test-lifecycle-guard.sh — run lifecycle-guard's own cargo tests.
#
#   ./test-lifecycle-guard.sh
#
# WHERE IT RUNS: the TIMED set, found by the `spira/test-*.sh` glob and run by `suites.sh`,
# because `gate-suites` does not name it. lifecycle-guard itself runs as a fence of the
# landing gate (gate.steps, sp-ts2qr), with no allow-list; test-lifecycle-guard-gate.sh proves
# that step refuses a planted violation.
#
# WHAT IS EXERCISED: each finding class the analyser makes (direct write, write reached
# through a wrapper — both the "$@"-forwarding and fixed-verb shapes, an unresolvable dynamic
# verb, a bd status read used in a conditional, a reference to the lifecycle credential/DSN
# surface, a retired label and a deleted state path once a rules file names one, and bd
# named in a brief) against a planted fixture (seen red), plus a clean fixture that reports
# nothing (seen green) and the exit-code contract the gate will read.
#
# defect: sp-ece5q
# tier: T1
# covers: lifecycle-guard/*
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
CRATE="$HERE/../lifecycle-guard"

# Resolve cargo/rustc BEFORE conf.sh (pulled in indirectly via lib.sh elsewhere in the
# gate) can overwrite PATH with the harness's own tool directories, which do not include
# ~/.cargo/bin — see test-spira-config.sh's own note; the same hazard applies here.
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
