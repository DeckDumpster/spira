#!/usr/bin/env bash
# tier: T3
# requires: testenv
# covers: sim/src/replay.rs sim/src/replay/** sim/src/bin/sim-replay.rs sim/tests/replay.rs sim/fixtures/replay/** desired-state/src/sections/** spira-lc/src/sections.rs spira-lc/src/sections/** spira-lc/src/keys.rs
# lane: sim
#
# test-sim-replay.sh — the captured days replayed whole on the simulator: `sim-replay` runs
# every check in sim::replay::checks (the stacked parent and the repeat-red member on 10-31,
# the held-out lanes' rounds against landings on 10-21..25, acks that change nothing, one seed
# one trace, hooks at their periods). Each costs seconds unoptimised, past the unit tier's
# second, so sim/tests/replay.rs ignores its copies and the installed binary runs them here.
#
#   CONTROL: every check the binary holds must report ok by name, so a binary that ran fewer,
#   or none, is red rather than a green that checked nothing.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

CHECKS=9

command -v sim-replay >/dev/null 2>&1 || bail "sim-replay is not on PATH"

out="$(sim-replay --fixtures "$(dirname "$HERE")/sim/fixtures/replay" 2>&1)"
rc=$?
printf '%s\n' "$out" | grep -E '^ok |^sim-replay:|panicked|assert'
is "every replay check passes" 0 "$rc"
is "every replay check ran" "$CHECKS" "$(printf '%s\n' "$out" | grep -c '^ok ')"
want "the binary reports all its checks" "sim-replay: $CHECKS checks passed" "$out"

tl_summary
