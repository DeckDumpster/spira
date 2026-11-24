#!/usr/bin/env bash
#
# bare-suites.sh — no bare-host suite remains; this stays so a round runner that still calls it
# passes. Delete it once no active round runner calls it.
#
# covers: spira/bare-suites.sh
set -uo pipefail
echo "bare-suites: checked 0 suites (no bare-host suite exists)"
exit 0
