#!/usr/bin/env bash
#
# test-spira-lc-crate.sh — spira-lc's own cargo tests: bd JSON parsing (bd_facts), the
# landstate/queue file readers (legacy_files), the ancestry/merge-tree content-on-base check
# (git_evidence — real scratch git repos, not a stub), and repository-configuration
# detection between spira.conf+repo-map and spira.toml (repo_config).
#
#   ./test-spira-lc-crate.sh
#
# host-reason: a pure `cargo test` run; git_evidence's own tests build real, throwaway git
# repositories in $TMPDIR, which need no container isolation of their own.
#
# defect: sp-t93ky
# tier: T0
# covers: spira-lc/*
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$HERE/../spira-lc"
. "$HERE/testlib.sh"

# Resolve cargo BEFORE conf.sh, which can overwrite PATH with the harness's own tool
# directories (see test-lifecycle-crate.sh's own note).
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin — install Rust: https://rustup.rs/"
GIT_BIN="$(command -v git 2>/dev/null || true)"
[ -n "$GIT_BIN" ] || skip "git not found on PATH — git_evidence's own tests need a real git binary"
export PATH="$(dirname "$CARGO_BIN"):$(dirname "$GIT_BIN"):$PATH"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
OUT="$TMP/cargo-test.out"
CARGO_TERM_COLOR=never "$CARGO_BIN" test --manifest-path "$CRATE/Cargo.toml" \
    --no-fail-fast > "$OUT" 2>&1
_rc=$?
cat "$OUT"
report_cargo "$OUT" "$_rc"

tl_summary
