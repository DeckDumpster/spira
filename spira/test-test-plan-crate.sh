#!/usr/bin/env bash
#
# test-test-plan-crate.sh — run test-plan's own cargo tests: catalogue validation (unknown
# field, bad tier, duplicate id, area/filename mismatch, each naming its path), unknown-UC
# and orphan-UC violations, tier-budget flags, matrix determinism, markdown rendering, and
# the T0 checks that the shipped example validates and the shipped JSON Schemas match the
# types.
#
# WHERE IT RUNS: the TIMED set, found by the `spira/test-*.sh` glob and run by `suites.sh`,
# because `gate-suites` does not name it — the same placement test-spira-config.sh uses while
# nothing in the gate depends on it directly. Here that is not quite true (plan-lint.sh and
# plan-matrix.sh both call the compiled binary), but the gate's own fence
# (spira/plan-matrix-fence.sh, run from gate-touched.sh) exercises the binary against the
# real tree on every branch already; this suite is the crate's unit/property coverage, not a
# second copy of the fence.
#
# tier: T1
# covers: test-plan/*
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$HERE/../test-plan"

pass=0; fail=0
ok()  { printf '  ok    %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n        %s\n' "$1" "${2:-}"; fail=$((fail+1)); }

# Resolve cargo/rustc BEFORE conf.sh (pulled in indirectly via lib.sh elsewhere in the gate)
# can overwrite PATH with the harness's own tool directories, which do not include
# ~/.cargo/bin — see test-loom.sh's and test-spira-config.sh's own note; the same hazard
# applies here.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-test-plan-crate: cargo not found on PATH or at ~/.cargo/bin" >&2
    echo "     install Rust: https://rustup.rs/" >&2
    exit 77
fi
export PATH="$(dirname "$CARGO_BIN"):$PATH"

if out="$("$CARGO_BIN" test --manifest-path "$CRATE/Cargo.toml" 2>&1)"; then
    ok "test-plan crate tests passed"
    printf '%s\n' "$out" | grep -E '^\s*(test |ok |FAILED|running)' || true
else
    bad "test-plan crate tests" "$(printf '%s\n' "$out" | tail -40)"
fi

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
