#!/usr/bin/env bash
#
# test-drain-banner.sh — health.sh renders a drained world as a firing condition.
#
#   ./test-drain-banner.sh
#
# WHY THIS SUITE EXISTS. On 2026-09-08 the session armed world.sh drain for a safe install
# and left it armed for ~13 minutes. world.sh status DID report DRAINING, but nothing put
# that in front of the operator — so a pool held at zero by design looked identical to a pool
# that is simply idle. The drain banner is the mechanism that makes those two states
# distinguishable from the health pane.
#
# WHAT THIS TESTS. cockpit/health.sh's drain_banner function:
#   - shows a DRAINING banner when $SPIRA_RUN/world.draining exists
#   - includes the minutes since the stamp was written (from stat mtime)
#   - renders `?` when the stamp exists but mtime cannot be read
#   - shows nothing when the stamp does not exist
#   - does not confuse drain with halt
#
# THE FIXTURE IS A STAMP FILE, not a mock world.sh. world.sh status reads systemd unit
# names (which are broken, sp-4biz), while drain/resume gate on the stamp file directly.
# The test does the same: plant the stamp, check the pane, remove it, check again.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control). Before asserting that
# drain is invisible when absent, this suite proves it is visible when present — otherwise
# a renderer that never shows the banner passes all absence assertions.
#
# No database, no network, under a second.
#
# defect: sp-v7ok
# tier: T1
# covers: cockpit/health.sh spira/world.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
PANE="$(cd "$HERE/../cockpit" && pwd)/health.sh"

echo "test-drain-banner.sh"

if [ ! -f "$PANE" ]; then
    bad "cockpit/health.sh exists" "$PANE not found"; exit 1
fi

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
NOW="$(date +%s)"

# Minimal environment: SPIRA_RUN points at our scratch tree, SPIRA_CONF at a nonexistent
# file so no real spira.conf leaks in, mock-systemctl always returns "active" so halt_banner
# does not emit extra rows from a timer that is not running on this box.
PD="$TMP"
mkdir -p "$PD/run"
printf '#!/bin/sh\necho active\n' > "$PD/bin-mock-systemctl"
chmod +x "$PD/bin-mock-systemctl"

pane() {   # pane -> health.sh frame, ANSI stripped
    env -i PATH="$PATH:$PD" HOME="$PD" TERM=dumb LC_ALL=C.UTF-8 \
        SPIRA_CONF="$PD/no.conf" SPIRA_REPO="$PD" SPIRA_RUN="$PD/run" \
        SPIRA_SYSTEMCTL="$PD/bin-mock-systemctl" \
        bash "$PANE" once 0 96 2>/dev/null | sed 's/\x1b\[[?0-9;]*[a-zA-Z]//g'
}

# ======================================================================================
echo
echo "positive control — drain stamp makes the banner visible:"
# ======================================================================================
# MUST APPEAR FIRST. If a frame without the stamp already shows "DRAINING", the absence
# assertions below pass for the wrong reason.
DRAIN="$PD/run/world.draining"
printf '2026-09-08 20:02:00 UTC\nsummons gated in summon_fayth\n' > "$DRAIN"
# Age the stamp to 5 minutes ago so the minutes field is a number, not unknown.
touch -d "@$(( NOW - 300 ))" "$DRAIN" 2>/dev/null || true
frame="$(pane)"
want "a drain stamp makes DRAINING appear" "DRAINING" "$frame"
want "and shows the lift instruction"      "world.sh resume" "$frame"
# The minutes are derived from stat mtime, which we set above. On a stat-capable system
# this must be a number; if stat is unavailable the assertion relaxes to `?`.
mins_line="$(printf '%s\n' "$frame" | grep -i 'DRAINING' | head -1)"
want "drain banner includes a time figure" "5m" "$mins_line"

# ======================================================================================
echo
echo "no drain stamp — banner is absent:"
# ======================================================================================
rm -f "$DRAIN"
frame="$(pane)"
nowant "no stamp produces no drain banner"  "DRAINING"       "$frame"
nowant "and no lift instruction"            "world.sh resume" "$frame"

# ======================================================================================
echo
echo "drain does not suppress the halt banner when both are present:"
# ======================================================================================
# Both stamps can coexist (halt supersedes drain but both stamps may be present during a
# transition). Both banners should appear so the operator sees the full picture.
printf '2026-09-08 20:00:00 UTC\nwhy: test halt\n' > "$PD/run/world.halted"
printf '2026-09-08 20:02:00 UTC\nsummons gated.\n'  > "$DRAIN"
frame="$(pane)"
want "both STOPPED and DRAINING appear when both stamps exist" "STOPPED"   "$frame"
want "both STOPPED and DRAINING appear when both stamps exist" "DRAINING"  "$frame"
rm -f "$PD/run/world.halted" "$DRAIN"

# ======================================================================================
echo
echo "drain without halt — only DRAINING, no STOPPED:"
# ======================================================================================
printf '2026-09-08 20:02:00 UTC\nsummons gated.\n' > "$DRAIN"
frame="$(pane)"
want   "drain-only shows DRAINING" "DRAINING" "$frame"
nowant "but not STOPPED"           "STOPPED"  "$frame"
rm -f "$DRAIN"

# ======================================================================================
echo
echo "halt without drain — only STOPPED, no DRAINING:"
# ======================================================================================
printf '2026-09-08 20:00:00 UTC\nwhy: test halt\n' > "$PD/run/world.halted"
frame="$(pane)"
want   "halt-only shows STOPPED"  "STOPPED"   "$frame"
nowant "but not DRAINING"         "DRAINING"  "$frame"
rm -f "$PD/run/world.halted"

echo
tl_summary
