#!/usr/bin/env bash
#
# dolt-boot-check.sh — alarm when the bead store's unit is not running some minutes after boot.
#
# A start job systemd drops (an ordering cycle) leaves the unit `inactive`, not `failed`, so no
# failed-unit probe sees it. Exits 1 after logging and a best-effort urgent mail; the red unit is
# the alarm that needs no store to be delivered.
#
# covers: spira/dolt-boot-check.sh systemd/spira-dolt-boot-check.service systemd/spira-dolt-boot-check.timer
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

unit="${SPIRA_DOLT_UNIT:-dolt-beads.service}"
state="$(systemctl --user is-active "$unit" 2>/dev/null)"
case "$state" in active|activating|reloading) exit 0 ;; esac

msg="$unit is '${state:-unknown}' at boot+check: the bead store is down. Run: systemctl --user start $unit"
echo "$(date -u +%FT%TZ) dolt-boot-check: $msg"
printf '%s\n' "$msg" | timeout 5 "${SPIRA_MAIL_BIN:-mail}" send operator --from "dolt-boot-check <dolt-boot-check@spira>" \
    --subject "bead store not running after boot" --kind note --urgent >/dev/null 2>&1 || true
exit 1
