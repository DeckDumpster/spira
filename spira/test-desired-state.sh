#!/usr/bin/env bash
#
# test-desired-state.sh — run desired-state's own cargo tests.
#
#   ./test-desired-state.sh
#
# WHERE IT RUNS: the TIMED set, found by the `spira/test-*.sh` glob and run by `suites.sh`,
# because `gate-suites` does not name it. Nothing consumes the desired-state document yet
# (the invariant engine bead does); this moves to the gate once a consumer does.
#
# WHAT IS EXERCISED: per-kind spec validation (unknown kind / unsupported apiVersion /
# unknown field / missing field / valid), the producer-fragment parser, the composer (merge,
# conflict refusal, unknown-kind refusal, a content hash stable across producer/time changes
# and sensitive to a spec change), the filesystem store (new version vs. unchanged, kept
# history), and the T0 check that the shipped canonical default validates against the shipped
# schema.
#
# defect: sp-xqhog
# covers: desired-state/*
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$HERE/../desired-state"

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
    echo "SKIP test-desired-state: cargo not found on PATH or at ~/.cargo/bin" >&2
    echo "     install Rust: https://rustup.rs/" >&2
    exit 77
fi
export PATH="$(dirname "$CARGO_BIN"):$PATH"

if out="$("$CARGO_BIN" test --manifest-path "$CRATE/Cargo.toml" 2>&1)"; then
    ok "desired-state tests passed"
    printf '%s\n' "$out" | grep -E '^\s*(test |ok |FAILED|running)' || true
else
    bad "desired-state tests" "$(printf '%s\n' "$out" | tail -40)"
fi

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
