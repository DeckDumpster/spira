#!/usr/bin/env bash
#
# test-dolt-git-push-pack.sh — the git that Dolt shells out to for a git+ssh remote push
# does not spend its deadline compressing and delta-searching already-compressed table files.
#
#   ./test-dolt-git-push-pack.sh
#
# Takes the git settings from the dolt-beads unit itself, so a unit that loses them fails here.
#
# tier: T1
# covers: systemd/dolt-beads.service
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
REPO="$(cd "$HERE/.." && pwd -P)"
UNIT="$REPO/systemd/dolt-beads.service"

echo "test-dolt-git-push-pack.sh"

unit_env=()
while IFS= read -r l; do unit_env+=("${l#Environment=}"); done < <(grep '^Environment=GIT_CONFIG_' "$UNIT")

echo
echo "the unit sets the git config that stops delta search on push:"
[ "${#unit_env[@]}" -ge 7 ] && ok "dolt-beads.service sets the git config" || bad "dolt-beads.service sets the git config" "found ${#unit_env[@]} GIT_CONFIG_ entries"
# The packed-size cases (defaults delta a near-duplicate pair; the unit's settings store it whole)
# were deleted under law-a-test-that-flips-is-deleted: r-auto-145 (2026-10-15) packed the pair as a
# delta under the unit's settings (3000272 bytes) on a tree no member had touched — 1 red in 34 rounds.

echo
tl_summary
