#!/usr/bin/env bash
#
# test-aeon-prompt-layers-sticking.sh — the thrash-banner rows of test-aeon-prompt-layers.sh,
# split off for wall time. It runs that file with PL_PART=sticking.
#
# tier: T2
# covers: aeon/src/* spira/chamber/*.fayth spira/lib.sh
set -uo pipefail
PL_PART=sticking
. "$(dirname "$0")/test-aeon-prompt-layers.sh"
