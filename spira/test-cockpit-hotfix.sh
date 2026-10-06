#!/usr/bin/env bash
#
# test-cockpit-hotfix.sh — the ops pane's HOTFIX banner reads SP_HOTFIX_LINE /
#   SP_HOTFIX_ALERT from the snapshot verbatim: silent with neither, a WARN-colored banner
#   with just the RUNNING UNLANDED line, escalated once ALERT joins it.
#
# cockpit.sh's own write of these two keys (from `release status`) is not this suite's
# job — it is a one-line pass-through covered by reading the pane side of the same
# contract here, and by release's own `cargo test -p release` for the text those keys
# carry (sp-6p20x, DESIGN.md "Hotfix").
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the past-threshold banner
# is proven to fire before any "stays silent" case is trusted.
#
# No database, no network.
#
# tier: T1
# covers: cockpit/ops/src/health.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
PANE="health"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "test-cockpit-hotfix.sh"

if ! command -v "$PANE" >/dev/null 2>&1; then
    bail "cannot find pane at $PANE"
fi

mkdir -p "$TMP/repo/.runtime/spira" "$TMP/home" "$TMP/bin"
printf '#!/bin/sh\necho active\n' > "$TMP/bin/mock-systemctl"
chmod +x "$TMP/bin/mock-systemctl"

tl_config SPIRA_RUN="$TMP/repo/.runtime/spira"
pane() {  # pane -> stripped rendering of a single `once` frame at SPIRA_RUN
    env -i PATH="$PATH" HOME="$TMP/home" TERM=dumb LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_REPO="$TMP/repo" \
        SPIRA_SYSTEMCTL="$TMP/bin/mock-systemctl" \
        SPIRA_TOML="$SPIRA_TOML" \
        "$PANE" once 40 100 2>/dev/null \
      | sed 's/\x1b\[[?0-9;]*[a-zA-Z]//g'
}

SHA="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
LINE="RUNNING UNLANDED $SHA: emergency fix (since 2026-09-30T00:00:00Z)"
ALERT="ALERT hotfix $SHA standing 5h >= threshold 4h"

# ======================================================================================
echo
echo "positive control — past threshold: the banner fires, escalated, carrying both lines:"
# ======================================================================================
printf "SP_NEXT_N=0\nSP_AWAITING_N=0\nSP_HOTFIX_LINE='%s'\nSP_HOTFIX_ALERT='%s'\n" "$LINE" "$ALERT" \
    > "$TMP/repo/.runtime/spira/cockpit.env"
past="$(pane)"
want "past threshold: the HOTFIX banner appears"       "HOTFIX"        "$past"
want "past threshold: escalated label"                 "PAST THRESHOLD" "$past"
want "past threshold: the RUNNING UNLANDED line appears" "$LINE"       "$past"
want "past threshold: the ALERT line appears"           "$ALERT"       "$past"
want "past threshold: names how to clear it"            "release rollback" "$past"

# ======================================================================================
echo
echo "under threshold — RUNNING UNLANDED alone: banner fires, not escalated:"
# ======================================================================================
printf "SP_NEXT_N=0\nSP_AWAITING_N=0\nSP_HOTFIX_LINE='%s'\nSP_HOTFIX_ALERT=''\n" "$LINE" \
    > "$TMP/repo/.runtime/spira/cockpit.env"
under="$(pane)"
want   "under threshold: the HOTFIX banner appears"        "HOTFIX"          "$under"
nowant "under threshold: not labelled past threshold"      "PAST THRESHOLD"  "$under"
want   "under threshold: the RUNNING UNLANDED line appears" "$LINE"          "$under"

# ======================================================================================
echo
echo "no hotfix standing — the banner stays silent:"
# ======================================================================================
printf "SP_NEXT_N=0\nSP_AWAITING_N=0\nSP_HOTFIX_LINE=''\nSP_HOTFIX_ALERT=''\n" \
    > "$TMP/repo/.runtime/spira/cockpit.env"
clean="$(pane)"
nowant "no hotfix: no HOTFIX banner" "HOTFIX" "$clean"

# ======================================================================================
echo
echo "the key is simply absent (an older snapshot) — same as empty, not a crash:"
# ======================================================================================
printf "SP_NEXT_N=0\nSP_AWAITING_N=0\n" > "$TMP/repo/.runtime/spira/cockpit.env"
absent="$(pane)"
nowant "absent key: no HOTFIX banner" "HOTFIX" "$absent"

echo
tl_summary
