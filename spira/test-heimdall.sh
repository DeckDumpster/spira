#!/usr/bin/env bash
#
# test-heimdall.sh — the watchdog's reconciler: raised, cleared and re-raised as control-plane
# conditions, against a real spira-lc on a throwaway control store.
#
#   ./test-heimdall.sh
#
# The classifier half (`heimdall classify`) is test-heimdall-classify.sh. This suite drives the
# real `spira-lc condition` verbs on a fresh SQLite control store, because a stub for the store
# has made correct callers look broken before (law-prefer-the-real-dependency). Only the fault
# cases put a shim in front of it, to fail the writes or time out on demand.
# tier: T2
# covers: heimdall/src/* spira/testlib/heimdall-fixture.sh spira-lc/src/control/state/conditions.rs spira-lc/src/control/state/conditions/* spira/conf.sh UC-ops-detection-remediation-19
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

command -v heimdall >/dev/null 2>&1 \
    || { echo "test-heimdall: heimdall is not on PATH" >&2; exit 1; }
REAL_LC="$(command -v spira-lc)" \
    || { echo "test-heimdall: spira-lc is not on PATH" >&2; exit 1; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "test-heimdall.sh"

. "$HERE/testlib/heimdall-fixture.sh"

echo "heimdall — one condition per cause, raised, cleared and re-raised:"

wedge
heimdall >/dev/null
is "one sighting does not fire — a condition must be confirmed" "-" "$(cond_status sentinel-stalled)"
[ -r "$RUN/heimdall.status" ] && ok "the heartbeat is written even when nothing fires" \
    || bad "heartbeat" "no $RUN/heimdall.status after a quiet run"

heimdall >/dev/null
is "a confirmed condition raises it True at conditions/heimdall/<key>" "True 1" "$(cond_status sentinel-stalled)"
# THE WRITER'S HALF OF A CONTRACT WITH THE ATTENTION PANE: type Alert, and the message's first
# line is the title the pane shows.
is "the condition is an Alert whose message leads with the title" "Alert True" \
   "$(cond_field sentinel-stalled type status)"
[ -n "$("$REAL_LC" condition list --owner heimdall | python3 -c '
import sys, json
c = [c for c in json.load(sys.stdin)["conditions"] if c["key"] == "sentinel-stalled"][0]
print(c["message"].split("\n")[0])')" ] && ok "the message's first line is a title" || bad "title" "empty title line"

heimdall >/dev/null
is "a condition that keeps holding is still one firing" "True 1" "$(cond_status sentinel-stalled)"

heal; heimdall >/dev/null
is "one clear sighting does not retract it" "True 1" "$(cond_status sentinel-stalled)"
heimdall >/dev/null
is "Heimdall clears its own alert when the condition passes" "False 1" "$(cond_status sentinel-stalled)"

wedge; heimdall >/dev/null; heimdall >/dev/null
is "the condition returning re-raises the SAME condition, one more flap" "True 2" "$(cond_status sentinel-stalled)"
is "a flap does not accumulate conditions" 1 "$(n_alerts)"

# `acked` says the operator has SEEN this occurrence, so a returning condition is unseen again;
# `silent_until` is the pane's own and survives.
"$REAL_LC" condition ack sentinel-stalled --owner heimdall >/dev/null 2>&1
"$REAL_LC" condition silence sentinel-stalled --until 4070908800 --owner heimdall >/dev/null 2>&1
heal; heimdall >/dev/null; heimdall >/dev/null       # clears
wedge; heimdall >/dev/null; heimdall >/dev/null      # and returns
is "a returning condition is unseen again and keeps its silence" "False 4070908800" \
   "$(cond_field sentinel-stalled acked silent_until)"

echo
echo "heimdall — the operator's own clear, and the channel of last resort:"

# Clearing it by hand is an acknowledgement; re-raising it would argue with the operator.
"$REAL_LC" condition clear sentinel-stalled --reason Acknowledged --owner heimdall >/dev/null 2>&1
sed -i 's/\t[0-9]*$/\t0/' "$RUN/heimdall.state"      # force the hourly refresh window open
heimdall >/dev/null; heimdall >/dev/null
is "an alert cleared by hand is not re-raised while it still fires" "False" "$(cond_field sentinel-stalled status)"

HEIMDALL_STORE=/nonexistent-control-store heimdall >/dev/null
[ -r "$RUN/heimdall.alerts.json" ] && ok "an unreachable store falls back to the file channel" \
    || bad "fallback" "no fallback file was written with the store down"
fb="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print("%s %s" % (d["db_reachable"], ",".join(sorted(a["key"] for a in d["alerts"]))))' \
    "$RUN/heimdall.alerts.json" 2>/dev/null)"
case "$fb" in "False "*db-unreachable*) ok "the fallback carries the alerts and says the store is down" ;;
    *) bad "fallback content" "got [$fb]" ;; esac
grep -q 'SP_HEIMDALL_DB_READ=down' "$RUN/heimdall.status" \
    && ok "the heartbeat is written even when the store is down" \
    || bad "heartbeat under failure" "SP_HEIMDALL_DB_READ is not down in $RUN/heimdall.status"

heimdall >/dev/null
[ -e "$RUN/heimdall.alerts.json" ] \
    && bad "fallback removal" "the fallback file survived the store coming back — two sources of truth" \
    || ok "the fallback file is removed once the store answers again"

echo
echo "heimdall — the write probe detects a store that reads but will not write:"

heal; rm -f "$RUN/heimdall.state"
heimdall >/dev/null
grep -q 'SP_HEIMDALL_DB_WRITE=ok' "$RUN/heimdall.status" \
    && ok "write probe: a healthy store shows write ok" \
    || bad "write probe baseline" "SP_HEIMDALL_DB_WRITE is not ok before the fault"
is "write probe: the probe is a condition that never fires" "WriteProbe False 0" \
   "$(cond_field heimdall-write-probe type status flaps)"
is "write probe: the probe is not listed among the alerts" 1 "$(n_alerts)"

printf write-fail > "$TMP/lc-mode"
heimdall >/dev/null
grep -q 'SP_HEIMDALL_DB_READ=ok' "$RUN/heimdall.status" \
    && ok "write probe: reads still ok during a write-only failure" \
    || bad "write probe read during fault" "SP_HEIMDALL_DB_READ is not ok with writes broken"
grep -q 'SP_HEIMDALL_DB_WRITE=down' "$RUN/heimdall.status" \
    && ok "write probe: SP_HEIMDALL_DB_WRITE=down when writes fail" \
    || bad "write probe fault" "SP_HEIMDALL_DB_WRITE is not down with writes broken"
fb_write="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(d.get("db_reachable"), d.get("db_write_ok"))' "$RUN/heimdall.alerts.json" 2>/dev/null)"
is "write probe: the fallback says reachable, not writable" "True False" "$fb_write"

printf ok > "$TMP/lc-mode"
heimdall >/dev/null
grep -q 'SP_HEIMDALL_DB_WRITE=ok' "$RUN/heimdall.status" \
    && ok "write probe: write ok restored after the fault clears" \
    || bad "write probe restore" "SP_HEIMDALL_DB_WRITE is not ok after the fault cleared"
[ ! -e "$RUN/heimdall.alerts.json" ] \
    && ok "write probe: the fallback file is removed once writes succeed again" \
    || bad "write probe fallback removal" "fallback file survived the store recovering"

echo
echo "heimdall — a timed-out store is saturated, not down:"

heal; rm -f "$RUN/heimdall.state"
printf timeout > "$TMP/lc-mode"
heimdall >/dev/null
grep -q 'SP_HEIMDALL_DB_READ=saturated' "$RUN/heimdall.status" \
    && ok "saturation: a timeout on the read probe reports DB_READ=saturated" \
    || bad "saturation read" "expected SP_HEIMDALL_DB_READ=saturated, got $(grep SP_HEIMDALL_DB_READ "$RUN/heimdall.status" 2>/dev/null || echo none)"
grep -q 'SP_HEIMDALL_DB_SATURATED=1' "$RUN/heimdall.status" \
    && ok "saturation: SP_HEIMDALL_DB_SATURATED=1 when timed out" \
    || bad "saturation flag" "SP_HEIMDALL_DB_SATURATED not 1"
[ -r "$RUN/heimdall.alerts.json" ] \
    && ok "saturation: the fallback file is written on a timeout" \
    || bad "saturation fallback" "no fallback file on timeout"
printf ok > "$TMP/lc-mode"

echo
echo "heimdall — restart loop detection via stub systemctl:"

# Build a stub systemctl that serves list-units and show responses so heimdall.sh sees
# a unit with a rising NRestarts count. RESTARTS_THRESHOLD is set to 3 so a count
# of 5 (above) and 2 (below) cleanly straddle the threshold.
RESTART_SC="$TMP/restart-sc"
{
    printf '#!/usr/bin/env bash\n'
    # Return a fixed count stored in a file so the test can change it between runs.
    printf 'COUNT_FILE=%q\n' "$TMP/restart-count"
    printf 'case "${*}" in\n'
    # list-units: return one spira service
    printf '  *"list-units"*)\n'
    printf '    printf "spira-cockpit-prod.service loaded active running\\n"\n'
    printf '    exit 0 ;;\n'
    # show --property=Id,NRestarts: return Id and current count
    printf '  *"show"*)\n'
    printf '    cnt=$(cat "$COUNT_FILE" 2>/dev/null || echo 0)\n'
    printf '    printf "Id=spira-cockpit-prod.service\\nNRestarts=%%s\\n\\n" "$cnt"\n'
    printf '    exit 0 ;;\n'
    # is-active (for sentinel timer): not active
    printf '  *) exit 0 ;;\n'
    printf 'esac\n'
} > "$RESTART_SC"
chmod +x "$RESTART_SC"

heimdall_restart() {
    tl_config SPIRA_RUN="$RUN" SPIRA_EXPORTER="" SPIRA_WIKI="" \
        SPIRA_HEIMDALL_RESTARTS=3 SPIRA_HEIMDALL_RESTART_WINDOW=3600 \
        SPIRA_LC_STORE_PATH="$STORE"
    SPIRA_HOME="$SH" \
    SPIRA_REPO="$TMP/repo" \
    SPIRA_SYSTEMCTL="$RESTART_SC" \
    SPIRA_HEIMDALL_SENTINEL_LOG="$RUN/sentinel.log" \
        command heimdall --home "$SH" "$@" 2>&1
}
restart_alert_status() { cond_status "restart-loop:spira-cockpit-prod.service"; }

heal  # healthy sentinel log so sentinel-stalled does not interfere
rm -f "$RUN/heimdall.state"

# Count below threshold: no alert.
printf '2\n' > "$TMP/restart-count"
heimdall_restart >/dev/null
[ "$(restart_alert_status)" = "-" ] \
    && ok "restart: count below threshold fires nothing" \
    || bad "restart below threshold" "alert raised when count was below threshold"

# Count above threshold but only one sighting: CONFIRM requires two.
printf '10\n' > "$TMP/restart-count"
heimdall_restart >/dev/null
[ "$(restart_alert_status)" = "-" ] \
    && ok "restart: one sighting does not fire — a condition must be confirmed" \
    || bad "restart confirm" "alert raised on first sighting above threshold"

# Second sighting: alert raised.
heimdall_restart >/dev/null
_rst="$(restart_alert_status)"
is "restart: a confirmed condition raises it True" "True 1" "$_rst"

# Third pass on the same standing loop: still one firing, no second flap.
heimdall_restart >/dev/null
is "restart: a second pass on the same standing loop is no new firing" "True 1" "$(restart_alert_status)"

# Count goes DOWN (daemon-reload): no new alert; baseline resets.
printf '0\n' > "$TMP/restart-count"
heimdall_restart >/dev/null; heimdall_restart >/dev/null
is "restart: the alert clears after the restart count went down" "False 1" "$(restart_alert_status)"

# Count rises again above threshold: alert reopened.
printf '8\n' > "$TMP/restart-count"
heimdall_restart >/dev/null; heimdall_restart >/dev/null
is "restart: the count rising again re-raises the same condition" "True 2" "$(restart_alert_status)"

unset _rst

echo
echo "heimdall — a deliberately halted loop does not raise sentinel-stalled:"

heal; rm -f "$RUN/heimdall.state"
printf '2026-01-01T00:00:00Z\nwhy: test halt\n' > "$RUN/world.halted"
wedge
heimdall >/dev/null; heimdall >/dev/null
is "a halted loop does not raise sentinel-stalled" "False" "$(cond_field sentinel-stalled status)"
rm -f "$RUN/world.halted"
heimdall >/dev/null; heimdall >/dev/null
is "lifting the halt stamp restores normal alerting" "True" "$(cond_field sentinel-stalled status)"

tl_summary
