#!/usr/bin/env bash
#
# test-batcher.sh — the native cargo job for the batcher crate's pure core (sp-xr8rf).
#
# The core has no IO: no git, no testenv-batch, no forge, no bead store, no wall clock. Its
# replay tests are ordinary `cargo test` unit tests, so this suite is test-cockpit-rust.sh's
# shape with the fixture database dropped — there is nothing here that needs one.
#
# `cargo test`'s stdout already names every test as it finishes (`test <path> ... ok` /
# `... FAILED`); this suite parses those lines and calls testlib.sh's ok/bad once per Rust
# test, so a red names the failing case instead of folding 40-odd tests into one line.
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

_report_cargo_tests() {
    local out="$1" line name detail
    while IFS= read -r line; do
        case "$line" in
            "test "*" ... ok")
                name="${line#test }"; name="${name% ... ok}"
                ok "$name"
                ;;
            "test "*" ... FAILED")
                name="${line#test }"; name="${name% ... FAILED}"
                detail="$(awk -v t="---- $name stdout ----" '
                    $0 == t { grab=1; next }
                    grab && /^----/ { exit }
                    grab && /^failures:/ { exit }
                    grab { print }
                ' "$out" | head -5 | tr '\n' ' ')"
                bad "$name" "${detail:-see cargo output above}"
                ;;
        esac
    done < "$out"
}

OUT="$TMP/cargo-test.out"
CARGO_TERM_COLOR=never "$CARGO_BIN" test --manifest-path "$ROOT/Cargo.toml" \
    -p batcher --no-fail-fast > "$OUT" 2>&1
cat "$OUT"
_report_cargo_tests "$OUT"

tl_summary
