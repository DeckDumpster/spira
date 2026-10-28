#!/bin/sh
# supersede-request.sh <bead> <successor> [after-landed] — the event `work superseded-by` applies: a manual
# hold with cause supersede-request. With `after-landed`, exit 3 until the successor row is LANDED.
set -u
bead="$1"; succ="$2"
if [ "${3:-}" = after-landed ]; then
    [ "$(spira-lc state "$succ" 2>/dev/null)" = LANDED ] || exit 3
fi
[ "$(spira-lc state "$bead" 2>/dev/null)" = READY ] || exit 0
[ -z "$(spira-lc holds "$bead" 2>/dev/null)" ] || exit 0
ver=$(spira-lc show "$bead" | python3 -c 'import json,sys; print(json.load(sys.stdin)["bead"]["version"])') || { echo "supersede-request: no version for $bead" >&2; exit 9; }
echo "supersede-request: $bead at version $ver" >&2
spira-lc event bead "$bead" --expect READY --version "$ver" --actor dwarf \
    --kind "{\"Hold\":{\"kind\":\"Manual\",\"cause\":\"supersede-request\",\"detail\":\"$succ\"}}" >/dev/null || { echo "supersede-request: the hold on $bead was refused" >&2; exit 9; }
