#!/usr/bin/env bash
# test-install-decide.sh — systemd/install.sh's per-unit apply decision, as a table.
#
# _unit_action <changed> <masked> <suspended> <disabled> <halted> <active> is what decides,
# for one already-rendered unit, whether install.sh writes/restarts it, leaves it alone, or
# enables it without starting it. Sourcing install.sh with SPIRA_INSTALL_LIB=1 stops the
# file right after the function is defined: no conf.sh, no systemd, no rendered DEST tree,
# no recording systemctl. This replaces the decision coverage duplicated across
# test-install-idempotent.sh (deleted), test-install-halt.sh (deleted), test-ctrl.sh's
# install tier (deleted) and test-install-aeons.sh's no-op/selective cases (trimmed) —
# cluster 1, docs/test-plan/instance-lifecycle.md.
#
# tier: T1
# covers: systemd/install.sh UC-instance-lifecycle-22 UC-instance-lifecycle-23
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-install-decide.sh"

SPIRA_INSTALL_LIB=1 . "$HERE/../systemd/install.sh"
if ! declare -F _unit_action >/dev/null; then
    bail "SPIRA_INSTALL_LIB=1 did not define _unit_action — install.sh's guard is broken"
fi

# changed masked suspended disabled halted active -> expected action.
# Named by the six states cluster 1's evidence names them by: changed, unchanged, masked,
# disabled, halted, suspended.
cases=(
    "unchanged, active: nothing to do|0 0 0 0 0 1|skip"
    "changed, active: enable + restart|1 0 0 0 0 1|restart"
    "changed, inactive (new unit): enable --now|1 0 0 0 0 0|enable-now"
    "unchanged, inactive: recover it with enable --now|0 0 0 0 0 0|enable-now"

    "masked wins over changed+active|1 1 0 0 0 1|masked"
    "masked wins over suspended+disabled+halted|0 1 1 1 1 1|masked"

    "suspended, changed: not enabled|1 0 1 0 0 1|suspended"
    "suspended, unchanged+inactive: still not enabled|0 0 1 0 0 0|suspended"
    "suspended outranks disabled and halted|0 0 1 1 1 1|suspended"

    "operator-disabled, changed: file left alone|1 0 0 1 0 1|operator-disabled"
    "operator-disabled, unchanged+active: file left alone|0 0 0 1 0 1|operator-disabled"
    "operator-disabled outranks halted|1 0 0 1 1 1|operator-disabled"

    "halted: enable without --now, changed+active|1 0 0 0 1 1|enable"
    "halted: enable without --now, unchanged+inactive|0 0 0 0 1 0|enable"
    "halted: enable without --now, changed+inactive|1 0 0 0 1 0|enable"
)

for row in "${cases[@]}"; do
    name="${row%%|*}"; rest="${row#*|}"
    args="${rest%%|*}"; want_action="${rest##*|}"
    got="$(_unit_action $args)"
    is "$name ($args)" "$want_action" "$got"
done

# POSITIVE CONTROL — each flag alone must be able to flip the outcome, or a function that
# ignored it would still pass a table where it never mattered. "changed" alone (rows 1 vs 2)
# and "active" alone (rows 2 vs 3) already do that; the "outranks" rows above are the same
# control for masked/suspended/disabled/halted precedence — a wrong check order fails one.

tl_summary
