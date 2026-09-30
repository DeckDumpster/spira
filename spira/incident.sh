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
# covers: incident/src/*.rs spira/conf.sh spira/lib.sh
#         systemd/spira-ops.service spira/install-intake.sh
set -uo pipefail
. "$(dirname "$0")/lib.sh"
exec incident "$@"
