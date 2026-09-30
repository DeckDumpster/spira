#!/usr/bin/env bash
# maechen-trigger.sh — thin shim; all logic is in the maechen-trigger binary (sp-0ekp7,
# wave 7b).
#
# RESOLVED VIA `command -v`, NEVER A BARE `exec maechen-trigger` (scar, sp-0ekp7). bash's
# `exec` builtin falls back to a CWD-relative lookup when its PATH search finds nothing —
# even with "." nowhere in PATH — and this checkout's own crate source directory,
# maechen-trigger/ at the repo root, collides with the binary's bare name whenever a
# caller's working directory is the checkout root (a systemd unit's WorkingDirectory=, a
# test harness invoked from the repo root, …). `command -v` does not have that fallback and
# correctly skips a same-named directory to keep searching PATH, so resolving through it
# first and exec'ing the absolute result — a path with a "/" in it — is exec-safe.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
bin="$(command -v maechen-trigger)" || {
    printf 'maechen-trigger.sh: maechen-trigger is not on PATH\n' >&2
    exit 1
}
exec "$bin" "$@"
