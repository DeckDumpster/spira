#!/bin/sh
# gate-mode.sh <bead> <pass|fail|no-verdict|infra> — steer the world's suite runner for one bead.
set -eu
echo "$1 $2" >> "$SIM_WORLD/config/gate-control"
