#!/usr/bin/env bash
# units-manifest.sh — the desired unit set for this install: install's own ENABLE array,
# one per-instance unit name per line, in the order units.sh builds it.
#
# The reconciler's Units invariant needs exactly this list to diff against `systemctl
# --user is-enabled/is-active`. Nothing here decides what should be enabled — that
# decision belongs to units.sh alone, and duplicating its array-building logic is how the
# two come to disagree. This sources the one copy install.sh itself uses.
#
# covers: systemd/units.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

# Resolve units.sh from the real install.sh's directory, the same way owned.sh does, so a
# worktree copy of this script still reads the installed harness's unit list.
_um_real_install="$(readlink -f "$HERE/../systemd/install.sh" 2>/dev/null \
    || printf '%s' "$HERE/../systemd/install.sh")"
. "$(dirname "$_um_real_install")/units.sh" || exit 1
unset _um_real_install

printf '%s\n' "${ENABLE[@]}"
