#!/usr/bin/env bash
# fleet-status.sh — observed fleet occupancy, for the reconciler's Fleet invariant.
#
#   fleet-status.sh
#
# One line per builder partition: "<labels>\t<ready>\t<live>\t<is-task>" — ready work
# claimable under that partition's predicate, aeons currently live under it, and whether
# it is served by the task pool (1) or by a lane fayth drawing outside it (0). A final
# line, "TOTAL\t<max-live-aeons>\t<live-lanes>\t<task-pool>", gives the fleet ceiling, how
# much of it lane fayths (ops/qa/groomer) currently hold, and SPIRA_MAX_AEONS itself.
#
# Reuses aeon_count/ready_count/fayth_partitions/spira_task_fayths exactly as
# sentinel.sh and watchtower.sh do, so this cannot disagree with what already decides
# who gets summoned (law-prefer-the-real-dependency).
#
# covers: spira/lib.sh spira/conf.sh spira/sentinel.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
. "$HERE/lib.sh"

TASK_FAYTHS=" $(spira_task_fayths) "
live_lanes=0
for f in $(spira_fayths); do
    case "$TASK_FAYTHS" in
        *" $f "*) ;;
        *) live_lanes=$(( live_lanes + $(aeon_count "$f" 2>/dev/null || echo 0) )) ;;
    esac
done

while IFS=$'\t' read -r labels exclude; do
    [ -n "$labels" ] || continue
    ready="$(ready_count "$labels" "$exclude" 2>/dev/null || echo 0)"
    live=0
    is_task=0
    for f in $(fayths_for_labels "$labels"); do
        live=$(( live + $(aeon_count "$f" 2>/dev/null || echo 0) ))
        case "$TASK_FAYTHS" in
            *" $f "*) is_task=1 ;;
        esac
    done
    printf '%s\t%s\t%s\t%s\n' "$labels" "$ready" "$live" "$is_task"
done < <(fayth_partitions)

printf 'TOTAL\t%s\t%s\t%s\n' "${SPIRA_MAX_LIVE_AEONS:-}" "$live_lanes" "${SPIRA_MAX_AEONS:-}"
