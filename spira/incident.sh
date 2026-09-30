#!/usr/bin/env bash
# incident.sh — thin shim; all logic is in the `incident` binary (sp-0ekp7, wave 7b).
# Sources lib.sh first, as the original script did, so every SPIRA_* default conf.sh
# resolves (SPIRA_RUN, SPIRA_DB, SPIRA_REPO_MAP, SPIRA_HOME_REPO, ...) is exported before
# the binary reads its environment. Kept at this path and name — rather than deleted
# outright — because several callers still reach it as a literal filename: watchtower.sh
# (sp-lnmbq, a concurrent rewrite this bead must not touch) does
# `command -v incident.sh`, and a few already-Rust callers wrap it in an explicit
# `bash <path>` (sentinel/check5.rs, reconciler/main.rs, testenv/suites/real.rs) — a
# two-line shell script satisfies both; a bare compiled binary named `incident.sh` is not
# possible (cargo refuses a `.` in a binary target name). See incident/DESIGN.md
# "Decisions".
#
# RESOLVED VIA `command -v`, NEVER A BARE `exec incident` (scar, sp-0ekp7 — the same fix
# maechen-trigger.sh needed). bash's `exec` builtin falls back to a CWD-relative lookup
# when its PATH search finds nothing — even with "." nowhere in PATH — and this
# checkout's own crate source directory, incident/ at the repo root, collides with the
# binary's bare name whenever a caller's working directory is the checkout root. `command
# -v` has no such fallback and correctly skips a same-named directory to keep searching
# PATH, so resolving through it first and exec'ing the absolute result is exec-safe.
#
# covers: incident/src/*.rs spira/conf.sh spira/lib.sh
#         systemd/spira-ops.service spira/install-intake.sh
set -uo pipefail
. "$(dirname "$0")/lib.sh"
bin="$(command -v incident)" || {
    printf 'incident.sh: incident is not on PATH\n' >&2
    exit 1
}
exec "$bin" "$@"
