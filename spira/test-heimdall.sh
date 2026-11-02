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
# covers: heimdall/src/* spira-lc/src/control/state/conditions.rs spira-lc/src/control/state/conditions/* spira/conf.sh UC-ops-detection-remediation-19
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

command -v heimdall >/dev/null 2>&1 \
    || { echo "test-heimdall: heimdall is not on PATH" >&2; exit 1; }
REAL_LC="$(command -v spira-lc)" \
    || { echo "test-heimdall: spira-lc is not on PATH" >&2; exit 1; }

# mklog — a synthetic sentinel log, in the format lib.sh's log() actually writes.
#   mklog <passes> <ready> <dwarves> <called:0|1> <complete:0|1> <last-pass-ends-at>
mklog() {
    N="$1" READY="$2" DWARVES="$3" SUMM="$4" DONE="$5" END="$6" EXTRA="${7:-}" python3 -c '
import datetime, os
n, end = int(os.environ["N"]), int(os.environ["END"])
out = []
for i in range(n):
    t = end - (n - 1 - i) * 120
    ts = datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    out.append("%s spira: state: open=20 plan_ready=%s in_progress=0 dwarves=%s runes=[builder ]"
               % (ts, os.environ["READY"], os.environ["DWARVES"]))
    if os.environ["EXTRA"]:
        out.append("%s spira: %s" % (ts, os.environ["EXTRA"]))
    if os.environ["SUMM"] == "1":
        out.append("%s spira: CHECK7 builder: %s ready, 1 free — calling" % (ts, os.environ["READY"]))
    if os.environ["DONE"] == "1":
        out.append("%s spira: pass complete — 0 action(s), 0 progress" % ts)
print("\n".join(out))'
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "test-heimdall.sh"

SH="$TMP/spira"; RUN="$TMP/run"; mkdir -p "$SH" "$RUN"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SH/"
# ITS OWN SPIRA_HOME AND ITS OWN repo-map, so nothing reads the operator's real repositories.
printf 'brain | %s | push | origin/main | |\n' "$TMP/repo" > "$SH/repo-map"

# A fresh control store: SQLite makes an empty file a new database. Direct mode, no service,
# and no lifecycle credential: SPIRA_LC_PASSWORD_FILE is a registered key, so undeclared it
# names the fixture home's absent credential, and spira-lc refuses before reaching the store.
STORE="$TMP/control.db"; : > "$STORE"
export SPIRA_LC_PASSWORD=unused
tl_config SPIRA_LC_SOCKET="" SPIRA_LC_PASSWORD_FILE="" SPIRA_LC_STORE_PATH="$STORE"

# The shim in front of the real spira-lc: `ok` passes through, `write-fail` refuses every
# write, `timeout` answers nothing as a timed-out call does.
mkdir -p "$TMP/shim"; printf ok > "$TMP/lc-mode"
cat > "$TMP/shim/spira-lc" <<SHIM
#!/usr/bin/env bash
case "\$(cat "$TMP/lc-mode")" in
    write-fail) [ "\${2:-}" = list ] || { echo "shim: write refused" >&2; exit 1; } ;;
    timeout) exit 124 ;;
esac
exec "$REAL_LC" "\$@"
SHIM
chmod +x "$TMP/shim/spira-lc"
export PATH="$TMP/shim:$PATH"

heimdall() {   # heimdall [--report] — one run against the fixture
    tl_config SPIRA_RUN="$RUN" SPIRA_EXPORTER="" SPIRA_WIKI="" SPIRA_LC_STORE_PATH="${HEIMDALL_STORE:-$STORE}"
    SPIRA_HOME="$SH" \
    SPIRA_REPO="$TMP/repo" SPIRA_SYSTEMCTL=true \
    SPIRA_HEIMDALL_SENTINEL_LOG="$RUN/sentinel.log" \
        command heimdall --home "$SH" "$@" 2>&1
}
cond_field() {   # cond_field <key> <field...> -> the fields space-joined, or "-" with no condition
    "$REAL_LC" condition list --owner heimdall 2>/dev/null | KEY="$1" python3 -c '
import sys, os, json
fields = sys.argv[1:]
try: rows = json.load(sys.stdin)["conditions"]
except Exception: rows = []
for c in rows:
    if c["key"] == os.environ["KEY"]:
        print(" ".join(str(c.get(f)) for f in fields)); break
else:
    print("-")' "${@:2}"
}
cond_status() { cond_field "$1" status flaps; }
n_alerts() {
    "$REAL_LC" condition list --owner heimdall --type Alert 2>/dev/null \
        | python3 -c 'import sys, json; print(len(json.load(sys.stdin)["conditions"]))' 2>/dev/null || echo "?"
}
# A REALISTIC WEDGE: passes completed normally until an hour ago, then kept starting and
# stopped finishing. A log with no completed pass ANYWHERE cannot distinguish a wedged
# loop from a box where Heimdall came up first, and the floor deliberately reads it as the
# second — so a fixture without completions would test the floor, not the wedge.
wedge()  { { mklog 6 5 1 1 1 "$(( $(date +%s) - 3600 ))"
             mklog 10 5 0 0 0 "$(date +%s)"; } > "$RUN/sentinel.log"; }
heal()   { mklog 10 5 1 1 1 "$(date +%s)" > "$RUN/sentinel.log"; }

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
echo "heimdall — deliberately halted loop does not raise sentinel-stalled:"

heal; rm -f "$RUN/heimdall.state"
printf '2026-01-01T00:00:00Z\nwhy: test halt\n' > "$RUN/world.halted"
wedge
heimdall >/dev/null; heimdall >/dev/null
# An earlier test already created a closed sentinel-stalled bead. The check is that no
# NEW OPEN bead was raised, not that no bead exists at all.
_st_halt="$(alert_status sentinel-stalled)"
case "$_st_halt" in
    "-"|*" closed") ok "a halted loop does not raise sentinel-stalled" ;;
    *) bad "halt suppress" "sentinel-stalled raised while loop is deliberately halted" ;;
esac
unset _st_halt
rm -f "$RUN/world.halted"
# Without the halt stamp, the same wedged log fires after CONFIRM=2 passes.
heimdall >/dev/null; heimdall >/dev/null
st_h="$(alert_status sentinel-stalled)"
case "$st_h" in *" open") ok "lifting the halt stamp restores normal alerting" ;;
    *) bad "halt lifted" "expected sentinel-stalled open after halt stamp removed, got [$st_h]" ;; esac
unset st_h

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
