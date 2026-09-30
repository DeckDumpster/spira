#!/usr/bin/env bash
#
# test-sop-gate-wired.sh — the gate is actually wired to call sop.sh lint.
#
# A validator nobody calls provides no protection. This is the difference between a fence
# that exists and a fence that runs (law-a-documented-control-must-exist); source greps
# only, so it costs nothing to run on every branch.
#
# NOT CURRENTLY WIRED (sp-hyc3a, sp-9mnvm). sop.sh lint used to run from gate-spira.sh,
# which had no caller since sp-b99nj (2026-09-26) and is deleted with the rest of that
# dead script. It is deliberately NOT re-homed into the harness's shipped gate command
# alongside the other nine fences gate-spira.sh carried: `sop applied` — and by extension
# `sop lint` — needs a real SPIRA_DB, which a fresh gate/CI runner never has (the same
# reason suite-state-fence.sh is not there either). sop.sh's own O(n^2) whitespace-strip
# over the shelf JSON (the other half of why this was never wired) is gone with the bash
# it lived in: the Rust port (sop crate, sp-8fsql) has no such cost. Re-add the "the gate
# calls sop lint" row only if the SPIRA_DB-in-CI constraint is ever lifted.
#
# tier: T0
# covers: sop/src/main.rs UC-operator-channel-44
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

want "lint appears in sop's own usage" "lint" "$(sop bogus-subcommand 2>&1)"

tl_summary
