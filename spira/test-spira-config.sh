#!/usr/bin/env bash
#
# test-spira-config.sh — run spira-config's own cargo tests.
#
#   ./test-spira-config.sh
#
# WHERE IT RUNS: the TIMED set, found by the `spira/test-*.sh` glob and run by `suites.sh`,
# because `gate-suites` does not name it. No consumer reads spira.toml yet (sp-upkae ships
# the crate only), so nothing depends on this at landing time; it moves to the gate once a
# consumer does.
#
# WHAT IS EXERCISED: schema validation per section (valid / unknown key / wrong type / bad
# enum / missing field), the spira.conf/repo-map/fayth converter's golden test, and the T0
# check that the shipped example validates against the shipped schema.
#
# defect: sp-upkae
# covers: spira-config/*
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$HERE/../spira-config"

pass=0; fail=0
ok()  { printf '  ok    %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n        %s\n' "$1" "${2:-}"; fail=$((fail+1)); }

# Resolve cargo/rustc BEFORE conf.sh (pulled in indirectly via lib.sh elsewhere in the
# gate) can overwrite PATH with the harness's own tool directories, which do not include
# ~/.cargo/bin — see test-loom.sh's own note; the same hazard applies here.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-spira-config: cargo not found on PATH or at ~/.cargo/bin" >&2
    echo "     install Rust: https://rustup.rs/" >&2
    exit 77
fi
export PATH="$(dirname "$CARGO_BIN"):$PATH"

if out="$("$CARGO_BIN" test --manifest-path "$CRATE/Cargo.toml" 2>&1)"; then
    ok "spira-config tests passed"
    printf '%s\n' "$out" | grep -E '^\s*(test |ok |FAILED|running)' || true
else
    bad "spira-config tests" "$(printf '%s\n' "$out" | tail -40)"
fi

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
