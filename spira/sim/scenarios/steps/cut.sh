#!/bin/sh
# cut.sh [with-release] — merge the open publish PR as GitHub would, then run the workflow's
# cut job on the merged head. Exit 3 until a publish PR is open, 9 if the cut job fails.
# With `with-release` the runner already has the release's tools on PATH; without, only what
# the job's own steps provide. SPIRA_TOML is supplied to both: the job's steps set none, and
# spira-config refuses without one.
set -u
pr=$(gh pr list --state open --json number -q '.[0].number // ""') || exit 9
[ -n "$pr" ] || exit 3
sha=$(gh pr view "$pr" --json headRefOid -q .headRefOid) || exit 9
branch=$(gh pr view "$pr" --json headRefName -q .headRefName) || exit 9
ws="$SIM_WORLD/ci-ws"
rm -rf "$ws"
git clone -q "$SIM_WORLD/origin.git" "$ws" || exit 9
git -C "$ws" checkout -q -B main "$sha" || exit 9
results=$(ls "$ws"/spira/test-*.sh | sed 's|.*/||; s|$|.result|' | paste -sd, -)
sim ghctl "$SIM_GH_DIR" check-run "$sha" gate completed success || exit 9
sim ghctl "$SIM_GH_DIR" run-add --branch "$branch" --sha "$sha" --conclusion success --artifact "batch-results-1:$results" >/dev/null || exit 9
sim ghctl "$SIM_GH_DIR" pr-merge "$pr" || exit 9
if [ "${1:-}" = with-release ]; then set -- --path "$SPIRA_RELEASE/bin"; else set --; fi
sim ci run "$ws/.github/workflows/gate.yml" cut --sha "$sha" --workspace "$ws" --skip "Mint App token for tag push" --env "SPIRA_TOML=$SPIRA_TOML" "$@" || exit 9
