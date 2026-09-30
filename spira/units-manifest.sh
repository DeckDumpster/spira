#!/usr/bin/env bash
# units-manifest.sh — the desired unit set for this install: the ENABLE set, one
# per-instance unit name per line, in build order.
#
# The reconciler's Units invariant needs exactly this list to diff against `systemctl
# --user is-enabled/is-active`. Nothing here decides what should be enabled — that
# decision belongs to install/src/manifest.rs alone (sp-31dm0: systemd/units.sh is
# retired), and duplicating its array-building logic is how the two come to disagree.
# This reads the one copy units-install itself uses.
#
# covers: install/src/manifest.rs spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

units-install --list-enable
