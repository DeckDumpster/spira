#!/usr/bin/env bash
#
# test-sop-gate-wired.sh — the gate is actually wired to call sop.sh lint.
#
# A validator nobody calls provides no protection. This is the difference between a fence
# that exists and a fence that runs (law-a-documented-control-must-exist); source greps
# only, so it costs nothing to run on every branch.
#
# tier: T0
# covers: spira/sop.sh spira/gate-spira.sh UC-operator-channel-44
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

want "gate-spira.sh calls sop.sh lint" "sop.sh lint" "$(cat "$HERE/gate-spira.sh")"
want "lint appears in sop.sh's own usage" "lint" "$(bash "$HERE/sop.sh" bogus-subcommand 2>&1)"

tl_summary
