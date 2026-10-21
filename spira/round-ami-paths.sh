#!/usr/bin/env bash
#
# round-ami-paths.sh — exit 0 when any changed path read from stdin is one the Round AMI
# workflow's push trigger lists, 1 when none is. The list is read from the workflow itself, so
# the trigger and this check cannot drift; an empty list is a refusal (2), never "no match".
#
#   git diff --name-only A..B | round-ami-paths.sh [workflow-file]
#
# covers: spira/round-ami-paths.sh .github/workflows/round-ami-build.yml .github/workflows/gate.yml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
wf="${1:-$HERE/../.github/workflows/round-ami-build.yml}"

mapfile -t paths < <(awk '/^  push:/{p=1;next} p&&/^  [a-z_]+:/{exit} p&&/^      - /{sub(/^      - /,"");print}' "$wf")
[ "${#paths[@]}" -gt 0 ] || { echo "round-ami-paths: no push paths found in $wf" >&2; exit 2; }

while IFS= read -r f; do
    for p in "${paths[@]}"; do
        [ "$f" = "$p" ] && exit 0
    done
done
exit 1
