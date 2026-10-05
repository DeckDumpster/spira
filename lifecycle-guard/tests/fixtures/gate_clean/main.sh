#!/usr/bin/env bash
# The gate's clean tree: the state comes from the machine, and a legacy bd write (a class the
# gate does not refuse yet) is reported on the summary line, not refused.
set -uo pipefail
state="$(spira-lc state "$1")"
[ "$state" = LANDED ] && echo landed
bd close "$1" --reason done
