#!/usr/bin/env bash
# queue-certified-list.sh <repo-name> — certified branches waiting to batch, for the
# reconciler's mergeability check.
#
#   queue-certified-list.sh <repo-name>
#
# One line per certified bead: "<bead-id> <branch> <base-ref>". Delegates to
# queue_certified_list, the same function batch.sh trusts to decide what it may cut into
# a batch, so this cannot see a different queue than batch.sh acts on.
#
# covers: spira/lib.sh spira/batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
. "$HERE/lib.sh"

name="${1:?usage: queue-certified-list.sh <repo-name>}"
repo="$(repo_root "$name")" || exit 1
base="$(spira_landref "$repo")" || exit 1

queue_certified_list "$repo" | while read -r id _tip _epoch; do
    printf '%s spira/%s %s\n' "$id" "$id" "$base"
done
