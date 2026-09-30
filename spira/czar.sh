#!/usr/bin/env bash
# czar.sh — thin shim; all logic is in the czar-pass binary (sp-54qsc).
#
# covers: czar-pass/src/main.rs spira/conf.sh spira/sentinel.sh watchtower/src/*
#         systemd/spira-czar-pass.service systemd/spira-czar-pass.timer
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
exec czar-pass "$@"
