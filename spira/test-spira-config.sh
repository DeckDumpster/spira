#!/usr/bin/env bash
#
# test-spira-config.sh — run spira-config's own cargo tests.
#
#   ./test-spira-config.sh
#
# WHERE IT RUNS: the TIMED set, found by the `spira/test-*.sh` glob and run by `suites.sh`,
# because `gate-suites` does not name it. conf.sh is now a consumer (sp-zs04v.2); the
# consumer's own coverage lives in test-conf-toml.sh, which does name this crate.
#
# WHAT IS EXERCISED: schema validation per section (valid / unknown key / wrong type / bad
# enum / missing field), the spira.conf/repo-map/fayth converter's golden test, and the T0
# check that the shipped example validates against the shipped schema.
#
# defect: sp-upkae
# tier: T1
# covers: spira-config/*
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$HERE/../spira-config"
. "$HERE/testlib.sh"

# Resolve cargo/rustc BEFORE conf.sh (pulled in indirectly via lib.sh elsewhere in the
# gate) can overwrite PATH with the harness's own tool directories, which do not include
# ~/.cargo/bin — see test-loom.sh's own note; the same hazard applies here.
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
