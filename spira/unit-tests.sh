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
# sp-mgmjhy: a build and its tests share the host with other builders, so neither
# claims every core. nproc inside a container reports the whole host.
cores="$(nproc 2>/dev/null || echo 2)"
jobs="${CARGO_BUILD_JOBS:-6}"
[ "$jobs" -le "$cores" ] 2>/dev/null || jobs="$cores"
threads="${RUST_TEST_THREADS:-4}"
# Test temp files go to the container's tmpfs /tmp, never the checkout's disk.
export TMPDIR=/tmp
cd "$ROOT" || exit 2
# The target is shared by every tree built in it and cargo trusts mtimes across them: the one
# selective clean (spira/target-clean.sh, sp-ke0uk8; sp-h6m43s, sp-ipwlic) cleans only the
# workspace crates whose bytes differ from the tree it was last built from, and records this one.
bash "$HERE/target-clean.sh" "$ROOT" || exit 2
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
# The round runs this tier with no declared release, config or harness home and no release
# on PATH (round-vm's unit_cargo: env -i); a test that passes only because the fixture's
# launcher declared them must fail here too.
unit_path=""
IFS=: read -ra _dirs <<< "$PATH"
for d in "${_dirs[@]}"; do
  [ -x "$d/spira-config" ] || [ -f "$d/conf.sh" ] || unit_path="${unit_path:+$unit_path:}$d"
done
env -u SPIRA_RELEASE -u SPIRA_TOML -u SPIRA_HOME PATH="$unit_path" \
    cargo test "${words[@]}" --features integration -j "$jobs" -- --test-threads="$threads" "${filters[@]}"
