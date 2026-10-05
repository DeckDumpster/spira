#!/usr/bin/env bash
# The gate's clean tree: the state comes from the machine, and nothing writes it around it.
set -uo pipefail
state="$(spira-lc state "$1")"
[ "$state" = LANDED ] && echo landed
