#!/usr/bin/env bash
#
# test-batcher-cut-wiring.sh — cases F (spira-lc wiring) and G (a flaky red) of test-batcher-cut.sh, split off for
# wall time. It runs that file with CUT_PART=wiring.
#
# tier: T2
# covers: batcher-cut/src/*.rs batcher/src/*.rs queue/src/* spira/conf.sh spira/lib.sh spira/bead.sh spira/runes/batcher.rune spira/runes/batcher.md spira/testlib/lc-fixture.sh
set -uo pipefail
CUT_PART=wiring
. "$(dirname "$0")/test-batcher-cut.sh"
