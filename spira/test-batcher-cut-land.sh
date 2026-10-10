#!/usr/bin/env bash
#
# test-batcher-cut-land.sh — the second half of test-batcher-cut.sh (cases G land mode
# onward), split off for wall time. It runs that file with CUT_PART=land.
#
# tier: T2
# covers: batcher-cut/src/*.rs batcher/src/*.rs queue/src/* spira/conf.sh spira/lib.sh spira/bead.sh spira/chamber/batcher.fayth spira/chamber/batcher.md spira/testlib/lc-fixture.sh
set -uo pipefail
CUT_PART=land
. "$(dirname "$0")/test-batcher-cut.sh"
