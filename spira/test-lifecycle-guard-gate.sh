#!/usr/bin/env bash
#
# test-lifecycle-guard-gate.sh — the landing gate's own call into lifecycle-guard
# (sp-sa8pn), not the analyser's own logic (that is test-lifecycle-guard.sh's job).
#
#   ./test-lifecycle-guard-gate.sh
#
# WHAT THIS PROVES:
#   - gate.steps builds and calls the analyser over the tree with no allowlist;
#   - gate.steps actually calls that binary against the tree and exits non-zero when it
#     finds something (SEEN RED, on lifecycle-guard's own "direct_write" fixture) — the
#     acceptance criterion "the gate fails a planted lifecycle write";
#   - a clean tree passes (SEEN GREEN), so the block is exercising the analyser and not
#     failing unconditionally.
#
# defect: sp-sa8pn
# tier: T2
# covers: gate.steps spira/conf.sh lifecycle-guard/**
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
REPO="$(cd "$HERE/.." && pwd)"

# Resolve cargo BEFORE conf.sh, which can overwrite PATH with the harness's own tool
# directories (~/.cargo/bin is not among them) — see test-lifecycle-guard.sh's own note.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin — install Rust: https://rustup.rs/"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
BUILD_LOG="$TMP/build.log"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$TMP/target" "$CARGO_BIN" build --manifest-path "$REPO/lifecycle-guard/Cargo.toml" --release --quiet \
    > "$BUILD_LOG" 2>&1 || bail "lifecycle-guard failed to build: $(cat "$BUILD_LOG")"

echo "test-lifecycle-guard-gate.sh"

BIN="$TMP/target/release/lifecycle-guard"
[ -x "$BIN" ] || bail "lifecycle-guard did not build to $BIN"

echo "gate.steps wires it:"
want "builds the analyser from the tree" "bin SPIRA_LIFECYCLE_GUARD_BIN lifecycle-guard" "$(cat "$REPO/gate.steps")"
want "runs it over the tree with no allowlist" 'step "$SPIRA_LIFECYCLE_GUARD_BIN" .' "$(cat "$REPO/gate.steps")"

echo
echo "clean tree passes:"
CLEAN="$TMP/clean"; mkdir -p "$CLEAN"
printf 'echo hi\n' > "$CLEAN/plain.sh"
"$BIN" "$CLEAN" >/dev/null 2>&1
wantrc "the gate's own call exits 0 on a clean tree" 0 $?

echo
echo "a planted lifecycle write fails the gate's call (SEEN RED):"
DIRTY="$TMP/dirty"; mkdir -p "$DIRTY"
cp "$REPO/lifecycle-guard/tests/fixtures/direct_write/"*.sh "$DIRTY/" 2>/dev/null || \
    bail "lifecycle-guard's own direct_write fixture is missing — cannot plant a positive control"
out="$("$BIN" "$DIRTY" 2>&1)"; rc=$?
wantrc "the gate's own call exits non-zero on the planted write" 1 "$rc"
want "and names what it found" "direct-write" "$out"

tl_summary
