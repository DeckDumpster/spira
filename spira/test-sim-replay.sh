#!/usr/bin/env bash
# tier: T3
# requires: testenv
# covers: sim/src/replay.rs sim/src/replay/** sim/tests/replay.rs sim/fixtures/replay/** desired-state/src/sections/** spira-lc/src/sections.rs spira-lc/src/sections/** spira-lc/src/keys.rs
# lane: sim
#
# test-sim-replay.sh — the captured days replayed whole on the simulator: sim/tests/replay.rs's
# slice-sized tests, which the unit tier skips (#[ignore]) because one 10-31 replay costs
# seconds, not the tier's sub-second budget. Run here, unoptimised, as the unit tier builds them.
#
#   CONTROL: the run must report every slice-sized test it names as run, so a filter that
#   matched nothing, or a test renamed away from it, is red rather than a green that ran zero.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
ROOT="$(dirname "$HERE")"

SLICE_TESTS=6

command -v cargo >/dev/null 2>&1 || bail "cargo is not on PATH"
cd "$ROOT" || bail "cannot enter $ROOT"

out="$(cargo test -p sim --test replay -j "$(nproc 2>/dev/null || echo 2)" -- --ignored 2>&1)"
rc=$?
printf '%s\n' "$out" | grep -E '^test |^test result|panicked|finished in'
is "the slice-sized replay tests pass" 0 "$rc"
want "every slice-sized test ran" "test result: ok. $SLICE_TESTS passed" "$out"

tl_summary
