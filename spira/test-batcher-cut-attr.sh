#!/usr/bin/env bash
#
# test-batcher-cut-attr.sh — cases B (local attribution) and G (a flaky red) of test-batcher-cut.sh, split off for
# wall time. It runs that file with CUT_PART=attr.
#
# tier: T2
# covers: batcher-cut/src/*.rs batcher/src/*.rs queue/src/* spira/conf.sh spira/lib.sh spira/bead.sh spira/runes/batcher.rune spira/runes/batcher.md spira/testlib/lc-fixture.sh
set -uo pipefail
CUT_PART=attr
. "$(dirname "$0")/test-batcher-cut.sh"
