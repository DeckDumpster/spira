#!/usr/bin/env bash
# maechen-trigger.sh — thin shim; all logic is in the maechen-trigger binary (sp-0ekp7,
# wave 7b).
#
# covers: maechen-trigger/src/*.rs spira/conf.sh spira/lib.sh
#         systemd/spira-maechen.service systemd/spira-maechen.timer
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
exec maechen-trigger "$@"
