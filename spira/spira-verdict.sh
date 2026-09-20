#!/usr/bin/env bash
# spira-verdict.sh — settle open merge-queue batches on a two-minute cadence.
#
# Iterates over every queue-mode repository and runs queue.sh step (verdict.sh
# then batch.sh) for each. Per-repo flock in verdict.sh and batch.sh prevents
# a concurrent landing pass from double-merging or double-ejecting.
#
# covers: spira/spira-verdict.sh spira/verdict.sh spira/batch.sh spira/queue.sh

set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

for _name in $(spira_repos); do
    [ "$(repo_land "$_name")" = queue ] || continue
    bash "$HERE/queue.sh" step "$_name" || true
done
