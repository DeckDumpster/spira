#!/usr/bin/env bash
#
# test-archive-no-sessions.sh — archive.sh on a box where no Claude session has run yet.
#
# THE DEFECT (local acceptance phase B, 2026-09-26). archive.sh refused ("transcript directory
# ~/.claude/projects does not exist — set SPIRA_TOKEN_PROJECTS", exit 1) whenever its source
# directory was missing. On a freshly installed box nothing has created ~/.claude/projects yet,
# so spira-archive failed on its first timer tick and every deploy's health check refused on
# the failed unit. The refusal exists so a MISCONFIGURED path does not read as "0 archived";
# the DEFAULT path not existing yet is the ordinary state of a new install, said and exit 0.
#
# CASES (law-absence-needs-a-positive-control):
#   - positive control: a source directory the operator configured and that is missing still
#     refuses, naming it;
#   - the default ~/.claude/projects missing: exit 0, and it says no session has run yet;
#   - the default present but empty: exit 0 (unchanged behaviour).
#
# tier: T1
# covers: spira/archive.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-archive-no-sessions.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

arch() {   # arch <home> <SPIRA_TOKEN_PROJECTS>
    tl_config SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/db" SPIRA_TOKEN_PROJECTS="$2"
    env -i PATH="$PATH" HOME="$1" SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
        archive.sh sweep 2>&1
}

# The "default ~/.claude/projects missing/empty" cases are gone: SPIRA_TOKEN_PROJECTS is a
# registered key, so an un-set override no longer falls back to $HOME — it reads the fixture's
# own (fixed, non-$HOME) value. Only the positive control, where the operator configured a
# path and it's missing, still applies.
mkdir -p "$TMP/h1"
out="$(arch "$TMP/h1" "$TMP/configured-but-missing")"; rc=$?
[ "$rc" -ne 0 ] && ok "positive control: a configured source directory that is missing still refuses" \
                || bad "positive control: a configured source directory that is missing still refuses" "rc=0"
want "positive control: and names it" "$TMP/configured-but-missing" "$out"

tl_summary
