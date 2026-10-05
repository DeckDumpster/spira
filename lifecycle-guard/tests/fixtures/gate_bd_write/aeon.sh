#!/usr/bin/env bash
# Planted around the lifecycle machine (sp-hyo5e): every one of these reaches a bead's state
# without spira-lc, and the gate refuses each class.
set -uo pipefail
release() { bdq update "$1" --status open --assignee ""; }
reopen_it() { bdq "$@"; }
finish() { release "$1"; reopen_it reopen "$1"; }
bd close "$1" --reason done
verb="$2"
bd "$verb" "$1"
[ "$(bd show "$1" --json)" = closed ] && echo closed
