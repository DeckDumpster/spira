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
# conf.sh sets SPIRA_HOME as a plain shell variable, never exported (it is read within
# THIS process, by scripts that source it directly) — but the binary below is a fresh
# process image after exec, and its own lib.sh seam (real.rs's `seam`/`seam_ok`) re-sources
# lib.sh in yet another subprocess, which needs SPIRA_HOME to find it at all. Exporting it
# here, to the one value that is always correct ($HERE, computed above from this script's
# own location, a lib.sh sibling), closes that gap — found via every seam call in
# test-maechen-trigger.sh failing uniformly (source-lib.sh rc=96, SPIRA_HOME seen as ".").
export SPIRA_HOME="$HERE"
exec "$bin" "$@"
