#!/usr/bin/env bash
#
# test-aeon-chamber-overlay-mech.sh — the overlay-mechanism rows of test-aeon-chamber-overlay.sh,
# split off for wall time. It runs that file with OV_PART=overlay.
#
# tier: T1
# covers: aeon/src/* spira/chamber/builder.md spira/conf.sh doctor/src/*
set -uo pipefail
OV_PART=overlay
. "$(dirname "$0")/test-aeon-chamber-overlay.sh"
