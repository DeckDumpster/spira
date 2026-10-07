#!/usr/bin/env bash
#
# test-aeon-prod-dirty-rest.sh — cases 2 and 3 of test-aeon-prod-dirty.sh, split off for wall
# time. It runs that file with PD_PART=rest.
#
# tier: T2
# covers: aeon/src/*
set -uo pipefail
PD_PART=rest
. "$(dirname "$0")/test-aeon-prod-dirty.sh"
