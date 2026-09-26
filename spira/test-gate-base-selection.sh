#!/usr/bin/env bash
# test-gate-base-selection.sh — the base trial selects what the branch's diff selects.
#
# Run against the base, a diff-selecting gate command sees an empty diff, selects nothing and
# passes, so every red suite reads as the branch's own. The gate therefore hands both trials
# the branch as the selection head.
# tier: T1
# covers: spira/gate.sh spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testlib/gate-fixture.sh"
command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
gate_fixture_init "$TMP"
BR=spira/sp-v1
gate_fixture_branch "$BR"
RUNS="$TMP/invocations"; : > "$RUNS"

# The command records which ref it was run at and which head it was told to select from, and
# is red at both, so the gate must run the base trial and report base-red.
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" \
    "echo \"\$SPIRA_GATE_BRANCH \${SPIRA_GATE_SELECT_HEAD:-unset}\" >> $RUNS; exit 1" > "$MAP"

echo "test-gate-base-selection.sh"
out="$(gate_fixture_run "$BR" repo SPIRA_VERDICT_TTL=600)"
is   "both trials ran"                          2 "$(wc -l < "$RUNS" | tr -d ' ')"
want "a command red on both is charged to the base" "reason=base-red" "$out"
is   "the branch trial selects from the branch" "$BR" "$(sed -n 1p "$RUNS" | cut -d' ' -f2)"
base_line="$(sed -n 2p "$RUNS")"
isnt_branch="$(printf '%s' "$base_line" | cut -d' ' -f1)"
[ "$isnt_branch" != "$BR" ] && ok "the second trial ran at the base" || bad "the second trial ran at the base" "$base_line"
is   "and the base trial also selects from the branch" "$BR" "$(printf '%s' "$base_line" | cut -d' ' -f2)"

echo
tl_summary
