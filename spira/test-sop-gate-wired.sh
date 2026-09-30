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
# dead script. It is deliberately NOT re-homed into repo-map.example alongside the other
# nine fences gate-spira.sh carried: sop.sh applied — and by extension sop.sh lint, which
# shares its O(n^2) whitespace-strip over the shelf JSON (sop.sh:406/660/799) — hangs past
# 120s under concurrent test-suite load (sp-oc2i6, sp-rjbrc). The fix (commit 43e57c268 on
# branch spira/sp-krqu0) has not landed on local/main. Wiring the fence in now would time
# out every gate. Re-add the "the gate calls sop.sh lint" row once that fix lands and this
# suite proves it under load.
#
# tier: T0
# covers: spira/sop.sh UC-operator-channel-44
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

want "lint appears in sop.sh's own usage" "lint" "$(bash "$HERE/sop.sh" bogus-subcommand 2>&1)"

tl_summary
