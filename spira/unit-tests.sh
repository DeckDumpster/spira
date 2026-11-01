#!/usr/bin/env bash
# unit-tests.sh — the Rust unit tests of the crates a tip's diff affects, in the fixture
# container. Not a corpus suite: it is run only as `testenv --suites unit-tests <tip> <repo>`,
# which leaves the crate list in target/unit-tests.crates and, under --tests, the test-name
# filters in target/unit-tests.filter.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(dirname "$HERE")"
LIST="$ROOT/target/unit-tests.crates"
[ -f "$LIST" ] || { echo "unit-tests: $LIST missing — run through testenv --suites unit-tests" >&2; exit 2; }
words=()
while IFS= read -r c; do [ -n "$c" ] && words+=(-p "$c"); done < "$LIST"
[ "${#words[@]}" -gt 0 ] || { echo "unit-tests: the diff touches no crate — nothing to run"; exit 0; }
jobs="$(nproc 2>/dev/null || echo 2)"
cd "$ROOT" || exit 2
# testenv mounts each tree's own target volume and names it in UNIT_TARGET_VOLUME
# (sp-fnb2wa), so the target holds no other tree's artifacts and stays warm. A testenv that
# names none mounts one target for every tree, and cargo trusts mtimes across them: workspace
# crates last built from different bytes are cleaned there (sp-h6m43s); dependencies stay warm.
# A tree that cannot be hashed is cleaned every time.
if [ -z "${UNIT_TARGET_VOLUME:-}" ]; then
  tree="$({ find . \( -path ./target -o -path ./.git \) -prune -o -type f -print0 | LC_ALL=C sort -z | xargs -0r sha256sum &&
            find . \( -path ./target -o -path ./.git \) -prune -o -type l -printf '%p -> %l\n' | LC_ALL=C sort; } | sha256sum)" || tree=""
  BUILT="${CARGO_TARGET_DIR:-$ROOT/target}/.unit-tests-built-tree"
  if [ -z "$tree" ] || [ "$(cat "$BUILT" 2>/dev/null)" != "$tree" ]; then
    rm -f "$BUILT" || exit 2
    cargo clean --workspace -q || { echo "unit-tests: cargo clean --workspace failed" >&2; exit 2; }
    [ -z "$tree" ] || printf '%s\n' "$tree" > "$BUILT" || exit 2
  fi
fi
BINS="$ROOT/target/unit-tests.bins"
[ -f "$BINS" ] || { echo "unit-tests: $BINS missing — run through testenv --suites unit-tests" >&2; exit 2; }
bins=()
while IFS= read -r c; do [ -n "$c" ] && bins+=(-p "$c"); done < "$BINS"
if [ "${#bins[@]}" -gt 0 ]; then
  cargo build "${bins[@]}" --bins -j "$jobs" || exit 1
fi
filters=()
if [ -f "$ROOT/target/unit-tests.filter" ]; then
  while IFS= read -r t; do [ -n "$t" ] && filters+=("$t"); done < "$ROOT/target/unit-tests.filter"
  echo "unit-tests: filtered to tests named ${filters[*]}"
fi
cargo test "${words[@]}" -j "$jobs" -- --test-threads="$jobs" "${filters[@]}"
