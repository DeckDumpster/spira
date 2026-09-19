#!/usr/bin/env bash
# broker.sh — thin shim; all logic is in the broker binary.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
exec "${SPIRA_BROKER_BIN}" "$@"
