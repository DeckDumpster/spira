#!/usr/bin/env bash
# test-dolt-boot-check.sh — a store unit that is not running after boot raises the alarm.
#
# tier: T1
# covers: spira/dolt-boot-check.sh systemd/spira-dolt-boot-check.service systemd/spira-dolt-boot-check.timer
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-dolt-boot-check.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/bin"
printf '#!/bin/sh\necho "${FAKE_UNIT_STATE:-inactive}"\n[ "${FAKE_UNIT_STATE:-inactive}" = active ]\n' > "$TMP/bin/systemctl"
printf '#!/bin/sh\ncat > %s/mailed\n' "$TMP" > "$TMP/bin/fakemail"
chmod +x "$TMP/bin/systemctl" "$TMP/bin/fakemail"
check() { PATH="$TMP/bin:$PATH" SPIRA_TOML="$(tl_layer)" SPIRA_DOLT_UNIT=my-store.service SPIRA_MAIL_BIN=fakemail \
    bash "$HERE/dolt-boot-check.sh" 2>&1; }

out="$(FAKE_UNIT_STATE=inactive check)"; rc=$?
wantrc "inactive unit exits 1" 1 $rc
want "names the unit" "my-store.service" "$out"
want "operator mailed" "bead store is down" "$(cat "$TMP/mailed" 2>/dev/null)"

rm -f "$TMP/mailed"
for st in active activating; do
    out="$(FAKE_UNIT_STATE=$st check)"; rc=$?
    wantrc "$st unit exits 0" 0 $rc
    [ ! -e "$TMP/mailed" ] && ok "$st sends no mail" || bad "$st sends no mail"
done

want "timer fires two minutes after boot" "OnBootSec=2min" "$(cat "$HERE/../systemd/spira-dolt-boot-check.timer")"
tl_summary
