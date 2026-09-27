#!/usr/bin/env bash
#
# work-env.sh <bead-id> [-- cmd args...]
#
# The aeon's restricted environment (design §3.5, this bead's own acceptance criterion:
# "In the provided aeon environment, command -v bd fails and no credential is readable").
# Deploys inert: nothing launches an aeon session through this yet — that is the aeon.sh
# cutover bead, sp-xethq. This exists so that bead can wire a session into something
# already built and already tested, rather than inventing the isolation at cutover time.
#
# `env -i` with an explicit allow-list, not `unset` on a deny-list: a deny-list is only as
# complete as whoever last remembered to update it, and a new credential-bearing var added
# anywhere else in this harness would leak through it by default. An allow-list leaks
# nothing new by construction — anything not named here is simply not there.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=conf.sh
. "$HERE/conf.sh"

bead_id="${1:-}"
[ -n "$bead_id" ] || { printf 'work-env.sh: bead id required\n' >&2; exit 2; }
shift || true
[ "${1:-}" = "--" ] && shift

work_bin_dir="$(dirname "$SPIRA_WORK_BIN")"
[ -x "$SPIRA_WORK_BIN" ] || {
    printf 'work-env.sh: %s is not built (SPIRA_WORK_BIN); cargo build -p work\n' "$SPIRA_WORK_BIN" >&2
    exit 2
}

cmd=("$@")
[ ${#cmd[@]} -eq 0 ] && cmd=("${SHELL:-/bin/bash}")

exec env -i \
    "HOME=$HOME" \
    "PATH=/usr/bin:/bin:$work_bin_dir" \
    "SPIRA_WORK_BEAD_ID=$bead_id" \
    "SPIRA_LC_SOCKET=$SPIRA_LC_SOCKET" \
    ${SPIRA_FAYTH:+"SPIRA_FAYTH=$SPIRA_FAYTH"} \
    ${TERM:+"TERM=$TERM"} \
    ${LANG:+"LANG=$LANG"} \
    ${GIT_AUTHOR_NAME:+"GIT_AUTHOR_NAME=$GIT_AUTHOR_NAME"} \
    ${GIT_AUTHOR_EMAIL:+"GIT_AUTHOR_EMAIL=$GIT_AUTHOR_EMAIL"} \
    ${GIT_COMMITTER_NAME:+"GIT_COMMITTER_NAME=$GIT_COMMITTER_NAME"} \
    ${GIT_COMMITTER_EMAIL:+"GIT_COMMITTER_EMAIL=$GIT_COMMITTER_EMAIL"} \
    ${SPIRA_AEON:+"SPIRA_AEON=$SPIRA_AEON"} \
    ${SPIRA_AEON_OVERRIDE:+"SPIRA_AEON_OVERRIDE=$SPIRA_AEON_OVERRIDE"} \
    ${SPIRA_RUN:+"SPIRA_RUN=$SPIRA_RUN"} \
    ${SPIRA_PROD:+"SPIRA_PROD=$SPIRA_PROD"} \
    ${BEAD_ID:+"BEAD_ID=$BEAD_ID"} \
    ${BEADS_ACTOR:+"BEADS_ACTOR=$BEADS_ACTOR"} \
    ${SPIRA_MAIL:+"SPIRA_MAIL=$SPIRA_MAIL"} \
    "${cmd[@]}"
