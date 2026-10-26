#!/usr/bin/env bash
#
# acceptance-waiver.sh — decide whether an acceptance run waives the upgrade phases (B/C/D).
#
# Usage: acceptance-waiver.sh --event <github event> [--input <true|false>] [--standing <value>]
#
# Prints `waive` or `run` on stdout and the reason on stderr. A workflow_dispatch run waives
# only on its own input. A tag push has no input, so it waives only when the standing
# setting (repo variable ACCEPTANCE_WAIVE_UPGRADE) is exactly `standing`; unset, the upgrade
# phases run.
set -uo pipefail

_event=""; _input="false"; _standing=""
while [ $# -gt 0 ]; do
    case "$1" in
        --event)    _event="${2:-}"; shift 2 ;;
        --input)    _input="${2:-}"; shift 2 ;;
        --standing) _standing="${2:-}"; shift 2 ;;
        *) printf 'acceptance-waiver: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

case "$_event" in
    workflow_dispatch)
        if [ "$_input" = "true" ]; then
            printf 'acceptance-waiver: upgrade phases waived by operator input\n' >&2
            echo waive
        else
            printf 'acceptance-waiver: upgrade phases run (no waiver input)\n' >&2
            echo run
        fi ;;
    push)
        if [ "$_standing" = "standing" ]; then
            printf 'acceptance-waiver: upgrade phases waived by standing ruling 2026-10-23 (ACCEPTANCE_WAIVE_UPGRADE=standing)\n' >&2
            echo waive
        else
            printf 'acceptance-waiver: upgrade phases run (ACCEPTANCE_WAIVE_UPGRADE not set to standing)\n' >&2
            echo run
        fi ;;
    *)
        printf 'acceptance-waiver: unknown event: %s\n' "$_event" >&2
        exit 2 ;;
esac
