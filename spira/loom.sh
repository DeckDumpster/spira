#!/usr/bin/env bash
#
# loom.sh — source the harness configuration, then start the Loom server.
#
# Systemd execs this so the binary reads its configuration from environment variables that
# conf.sh sets: SPIRA_LC_BIN, SPIRA_LOOM_ADDR, SPIRA_PATH and friends. A unit with a hardcoded
# SPIRA_LC_BIN works on exactly one box; a wrapper that sources conf.sh works on any.
set -uo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

command -v loom >/dev/null 2>&1 || { printf 'loom: not found on PATH (%s)\n' "$PATH" >&2; exit 2; }

exec loom "$@"
