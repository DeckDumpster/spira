#!/usr/bin/env bash
# reconciler.sh — thin shim; all logic is in the reconciler binary (sp-ocmes).
#
# covers: reconciler/src/main.rs reconciler-engine/src/*.rs spira/conf.sh
#         spira/units-manifest.sh spira/fleet-status.sh spira/queue-certified-list.sh
#         systemd/spira-reconciler.service systemd/spira-reconciler.timer
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
if [ ! -x "${SPIRA_RECONCILER_BIN:-}" ]; then
    printf 'reconciler: binary not found at %s\n' "${SPIRA_RECONCILER_BIN:-<unset>}" >&2
    printf 'reconciler: run: make build (in %s)\n' "$SPIRA_REPO" >&2
    exit 2
fi
exec "${SPIRA_RECONCILER_BIN}" "$@"
