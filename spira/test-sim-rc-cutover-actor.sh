#!/usr/bin/env bash
# test-sim-rc-cutover-actor.sh — the cutover drill's actor wrapper can fail: planted actors that
# claim, that run without naming the stop, and that name it are each judged as they should be.
# tier: T1
# covers: spira/sim/scenarios/steps/rc-cutover-actor.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
ACTOR="$HERE/sim/scenarios/steps/rc-cutover-actor.sh"

W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT INT TERM
export SPIRA_RUN="$W/run" SIM_WORLD="$W/world"
mkdir -p "$SPIRA_RUN" "$SIM_WORLD"
: > "$SIM_WORLD/claim-order"

run_actor() { sh "$ACTOR" "$@" >"$W/out" 2>"$W/err"; echo $?; }

is "outside cutover mode a non-land, non-claims actor does not run" "0" "$(run_actor heimdall sh -c 'echo ran >&2; exit 5')"
is "outside cutover mode the actor's own words are not judged" "0" "$(run_actor claims sh -c 'echo anything')"

: > "$SPIRA_RUN/world.cutover-mode"
is "under cutover mode an actor that skips under its stop passes" "0" "$(run_actor dwarves sh -c 'echo "builder: skipped — the dwarves lane of sim is stopped on purpose by cutover-mode"')"
is "under cutover mode an actor that runs without naming its stop fails" "9" "$(run_actor dwarves sh -c 'echo working')"
is "a stop of another lane is not the actor's own" "9" "$(run_actor heimdall sh -c 'echo "x: skipped — the dwarves lane of sim is stopped"')"
is "a claims actor that skips and claims nothing passes" "0" "$(run_actor claims sh -c 'echo "call-only fleet: skipped — the claims lane of sim is stopped"')"
is "a claims actor that claims under cutover mode fails whatever it says" "9" "$(run_actor claims sh -c 'echo "call-only fleet: skipped — the claims lane of sim"; echo "sp-x holder 1 builder" >> "$SIM_WORLD/claim-order"')"
is "a claims actor that does not name its stop fails" "9" "$(run_actor claims sh -c 'echo calling')"

tl_summary
