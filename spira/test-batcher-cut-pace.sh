#!/usr/bin/env bash
#
# test-batcher-cut-pace.sh — case N (pacing: a pool of one cuts despite a year-long wait) of test-batcher-cut.sh, split off for
# wall time. It runs that file with CUT_PART=pace.
#
# tier: T2
# covers: batcher-cut/src/*.rs batcher/src/*.rs queue/src/* spira/conf.sh spira/lib.sh spira/bead.sh spira/runes/batcher.rune spira/runes/batcher.md spira/testlib/lc-fixture.sh
set -uo pipefail
CUT_PART=pace
. "$(dirname "$0")/test-batcher-cut.sh"
