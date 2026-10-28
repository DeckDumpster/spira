#!/usr/bin/env bash
#
# test-dwarf-prod-dirty-rest.sh — cases 2 and 3 of test-dwarf-prod-dirty.sh, split off for wall
# time. It runs that file with PD_PART=rest.
#
# tier: T2
# covers: dwarf/src/*
set -uo pipefail
PD_PART=rest
. "$(dirname "$0")/test-dwarf-prod-dirty.sh"
