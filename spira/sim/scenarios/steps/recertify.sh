#!/bin/sh
# recertify.sh <bead> — send a second passing verdict for a bead the lifecycle already certified.
# Exit 3 until the bead is CERTIFIED, 9 if the repeat is refused, 0 when it is accepted.
set -u
bead="$1"
state=$(sim probe "$SIM_WORLD" | tr -d ' ' | grep "\"bead\":\"$bead\"" | grep -o '"lc_state":"[A-Z_]*"')
[ "$state" = '"lc_state":"CERTIFIED"' ] || exit 3
tip=$(git -C "$SIM_WORLD/work" rev-parse "refs/heads/spira/$bead") || exit 9
spira-lc certify "$bead" "$tip" pass gate || exit 9
