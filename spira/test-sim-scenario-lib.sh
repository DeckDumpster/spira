#!/usr/bin/env bash
# tier: T1
# covers: spira/sim-scenario-lib.sh
#
# test-sim-scenario-lib.sh — the diagnostics sim-scenario-lib.sh prints for a red scenario run.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/world"
printf 'sim: world kept at %s\n' "$T/world" > "$T/res.out"
cat > "$T/world/exec.log" <<'LOG'
=== t=700000 bash steps/s.sh sim a b: exit status: 3 in 20ms
runbook hand-land:2: round-recert --drive --batch rc-cm --repo sim -> 1
runbook hand-land:2: round-recert: rc-cm is not green: first
=== t=710000 bash steps/s.sh sim a b: exit status: 3 in 20ms
runbook hand-land:2: round-recert --drive --batch rc-cm --repo sim -> 1
runbook hand-land:2: round-recert: LAND FAILED for rc-cm
=== t=715000 other step: exit status: 0 in 5ms
quiet output
=== t=720000 other step: exit status: 9 in 5ms
a wrong answer
LOG

out="$(sim_last_waits "$T/res.out")"
want "the last exit-3 run of a step prints its own output" "LAND FAILED for rc-cm" "$out"
nowant "an earlier exit-3 run of the same step is not repeated" "is not green: first" "$out"
nowant "a run that exited 0 is not a wait" "quiet output" "$out"
nowant "a run that exited 9 is for sim_failed_commands" "a wrong answer" "$out"
is "a run with no kept world prints nothing" "" "$(sim_last_waits "$T/none.out")"
tl_summary
