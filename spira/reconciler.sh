#!/usr/bin/env bash
# reconciler.sh — thin shim; all logic is in the reconciler binary (sp-ocmes).
#
# covers: reconciler/src/*.rs reconciler-engine/src/*.rs reconciler-engine/src/*/*.rs spira/conf.sh
#         spira/units-manifest.sh spira/fleet-status.sh spira/queue-certified-list.sh
#         spira/disk-usage.sh spira/disk-remedy.sh
#         systemd/spira-reconciler.service systemd/spira-reconciler.timer
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
exec reconciler "$@"
