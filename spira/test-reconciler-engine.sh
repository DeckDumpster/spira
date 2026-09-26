#!/usr/bin/env bash
#
# test-reconciler-engine.sh — run reconciler-engine's own cargo tests.
#
#   ./test-reconciler-engine.sh
#
# WHERE IT RUNS: the TIMED set, found by the `spira/test-*.sh` glob and run by `suites.sh`,
# because `gate-suites` does not name it — the same shape as test-desired-state.sh, its
# sibling pure-core crate.
#
# WHAT IS EXERCISED: the pure invariant engine's `step` — satisfied stays satisfied, an
# unobservable reading never becomes satisfied (even past its grace period), a gap inside its
# grace period raises nothing, a gap past grace counts with its true since-when, a since_hint
# overriding first-noticed time, one clean pass closing a gap, a flap not closing early, a
# remedy that closes its gap vs. one that doesn't (escalates on the next pass, and the failed
# marker does not leak across a fresh streak), and a full multi-tick replay trace. Plus io.rs's
# own persistence round-trip tests.
#
# defect: sp-pu7v6
# covers: reconciler-engine/*
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$HERE/../reconciler-engine"

pass=0; fail=0
ok()  { printf '  ok    %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n        %s\n' "$1" "${2:-}"; fail=$((fail+1)); }

# Resolve cargo/rustc BEFORE conf.sh (pulled in indirectly via lib.sh elsewhere in the
# gate) can overwrite PATH with the harness's own tool directories, which do not include
# ~/.cargo/bin — see test-spira-config.sh's own note; the same hazard applies here.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-reconciler-engine: cargo not found on PATH or at ~/.cargo/bin" >&2
    echo "     install Rust: https://rustup.rs/" >&2
    exit 77
fi
export PATH="$(dirname "$CARGO_BIN"):$PATH"

if out="$("$CARGO_BIN" test --manifest-path "$CRATE/Cargo.toml" 2>&1)"; then
    ok "reconciler-engine tests passed"
    printf '%s\n' "$out" | grep -E '^\s*(test |ok |FAILED|running)' || true
else
    bad "reconciler-engine tests" "$(printf '%s\n' "$out" | tail -40)"
fi

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
