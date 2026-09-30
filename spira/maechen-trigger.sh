#!/usr/bin/env bash
# maechen-trigger.sh — thin shim; all logic is in the maechen-trigger binary (sp-0ekp7,
# wave 7b).
#
# covers: maechen-trigger/src/*.rs spira/conf.sh spira/lib.sh
#         systemd/spira-maechen.service systemd/spira-maechen.timer
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
echo "DEBUG maechen-trigger.sh: PATH=$PATH" >&2
echo "DEBUG maechen-trigger.sh: type=$(type -a maechen-trigger 2>&1)" >&2
echo "DEBUG maechen-trigger.sh: command -v=$(command -v maechen-trigger 2>&1)" >&2
IFS=: read -ra _dbg_parts <<< "$PATH"
for _p in "${_dbg_parts[@]}"; do
    if [ -e "$_p/maechen-trigger" ]; then
        echo "DEBUG maechen-trigger.sh: found at $_p/maechen-trigger -> $(ls -la "$_p/maechen-trigger" 2>&1)" >&2
    fi
done
exec maechen-trigger "$@"
