#!/usr/bin/env bash
#
# test-batcher-cut-base.sh — cases H (a red base), C (a stale member) and E (judgement-ci) of test-batcher-cut.sh, split off for
# wall time. It runs that file with CUT_PART=base.
#
# tier: T2
# covers: batcher-cut/src/*.rs batcher/src/*.rs queue/src/* spira/conf.sh spira/lib.sh spira/bead.sh spira/runes/batcher.rune spira/runes/batcher.md spira/testlib/lc-fixture.sh
set -uo pipefail
CUT_PART=base
. "$(dirname "$0")/test-batcher-cut.sh"
