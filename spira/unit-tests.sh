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
# testenv reads this copy of the clean's report. The container's user cannot remove testenv's
# file, so there an old copy is emptied instead.
COPY="$ROOT/target/unit-tests.clean.json"
rm -f "$COPY" 2>/dev/null || : > "$COPY"
bash "$HERE/target-clean.sh" "$ROOT"
cleaned=$?
REPORT="${CARGO_TARGET_DIR:-$ROOT/target}/.target-clean-report.json"
[ ! -f "$REPORT" ] || cat "$REPORT" > "$COPY"
[ "$cleaned" -eq 0 ] || exit 2
BINS="$ROOT/target/unit-tests.bins"
[ -f "$BINS" ] || { echo "unit-tests: $BINS missing — run through testenv --suites unit-tests" >&2; exit 2; }
bins=()
while IFS= read -r c; do [ -n "$c" ] && bins+=(-p "$c"); done < "$BINS"
# The lease reports what this took (spira-config build_dir): builds only, never the tests.
build_from=$SECONDS
if [ "${#bins[@]}" -gt 0 ]; then
  cargo build "${bins[@]}" --bins -j "$jobs" || exit 1
fi
filters=()
if [ -f "$ROOT/target/unit-tests.filter" ]; then
  while IFS= read -r t; do [ -n "$t" ] && filters+=("$t"); done < "$ROOT/target/unit-tests.filter"
  echo "unit-tests: filtered to tests named ${filters[*]}"
fi
skips=()
if [ -f "$ROOT/target/unit-tests.skip" ]; then
  names=()
  while IFS= read -r t; do [ -n "$t" ] && skips+=(--skip "$t") && names+=("$t"); done < "$ROOT/target/unit-tests.skip"
  [ "${#names[@]}" -eq 0 ] || echo "unit-tests: skipped (needs a running system): ${names[*]}"
fi
# Each line is `::<id>`: libtest's --skip matches it as a substring in every binary.
if [ -f "$ROOT/target/unit-tests.deny" ]; then
  denied=()
  while IFS= read -r t; do [ -n "$t" ] && skips+=(--skip "$t") && denied+=("$t"); done < "$ROOT/target/unit-tests.deny"
  [ "${#denied[@]}" -eq 0 ] || echo "unit-tests: left out (test registry deny list): ${denied[*]}"
fi
# The round runs this tier with no declared release, config or harness home and no release
# on PATH (round-vm's unit_cargo: env -i); a test that passes only because the fixture's
# launcher declared them must fail here too.
unit_path=""
IFS=: read -ra _dirs <<< "$PATH"
for d in "${_dirs[@]}"; do
  [ -x "$d/spira-config" ] || [ -f "$d/conf.sh" ] || unit_path="${unit_path:+$unit_path:}$d"
done
BUILD="$ROOT/target/unit-tests.build"
[ -f "$BUILD" ] || { echo "unit-tests: $BUILD missing — run through testenv --suites unit-tests" >&2; exit 2; }
build=()
while IFS= read -r w; do [ -n "$w" ] && build+=("$w"); done < "$BUILD"
env -u SPIRA_RELEASE -u SPIRA_TOML -u SPIRA_HOME PATH="$unit_path" CARGO_BUILD_JOBS="$jobs" \
    "${build[@]}" || exit 1
# Into debug/, following it when it links into a leased folder. The first build wins.
profile="${CARGO_TARGET_DIR:-$ROOT/target}/debug"
noted="$profile/.lease-first-build-secs"
[ -e "$noted" ] || { mkdir -p "$profile" && echo "$((SECONDS - build_from))" > "$noted"; } ||
  echo "unit-tests: could not record the build's seconds in $noted" >&2
env -u SPIRA_RELEASE -u SPIRA_TOML -u SPIRA_HOME PATH="$unit_path" \
    "$ROOT/tools/test-sandbox/runner" --around cargo test "${words[@]}" --no-fail-fast --features integration -j "$jobs" -- --test-threads="$threads" "${filters[@]}" "${skips[@]}"
rc=$?
# What this run built came from the tree it recorded. A run killed before here keeps the old
# stamp, so the next one rebuilds what it built.
bash "$HERE/target-clean.sh" --stamp "$ROOT" || exit 2
exit "$rc"
