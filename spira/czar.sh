#!/usr/bin/env bash
# czar.sh — thin shim; all logic is in the czar-pass binary (sp-54qsc).
#
# covers: czar-pass/src/main.rs spira/conf.sh spira/sentinel.sh spira/watchtower.sh
#         spira/systemd/spira-czar-pass.service spira/systemd/spira-czar-pass.timer
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
if [ ! -x "${SPIRA_CZAR_PASS_BIN:-}" ]; then
    printf 'czar: binary not found at %s\n' "${SPIRA_CZAR_PASS_BIN:-<unset>}" >&2
    printf 'czar: run: %s/build.sh\n' "$SPIRA_REPO" >&2
    exit 2
fi
exec "${SPIRA_CZAR_PASS_BIN}" "$@"
