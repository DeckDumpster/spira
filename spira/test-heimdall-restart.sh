#!/usr/bin/env bash
#
# test-heimdall-restart.sh — restart-loop detection against a stub systemctl, and the halted-loop suppression.
#
#   ./test-heimdall.sh
#
# The classifier half (`heimdall classify`, the pure function that turns observations into
# firing keys) is test-heimdall-classify.sh, which needs nothing but the heimdall binary and runs
# everywhere. This suite drives a real `bd` against a throwaway Dolt database, because a
# stub for bd has twice made correct callers look broken (law-prefer-the-real-dependency).
# With no server, this suite exits 77 — the automake skip convention, which gate-brain.sh
# names in the gate's output — but ONLY if everything that did run passed. A skip must
# never be able to swallow a failure.
#
# mklog (a synthetic sentinel log in lib.sh's log() format) is needed here too, for the
# wedge()/heal() fixtures below — it is duplicated from test-heimdall-classify.sh rather than
# shared, since testlib.sh suites are sourced standalone with no shared-helper convention.
#
# heimdall.sh is retired into the `heimdall` binary (sp-zpaq0, rewrite wave 5); this suite now
# drives that binary (`command heimdall --home "$SH"`) instead of `bash "$SH/heimdall.sh"`,
# exactly test-dwarf-sweep.sh's own `command dwarf --home ...` pattern for the same reason
# (the wrapper function below is itself named `heimdall`, shadowing the bare binary name).
# Three files share this setup: each heimdall pass costs about a second of real bd round trips,
# so the sections run as test-heimdall.sh, test-heimdall-probe.sh and test-heimdall-restart.sh,
# each on a fixture of its own.
# tier: T2
# covers: heimdall/src/* spira/testlib/heimdall-fixture.sh spira/conf.sh UC-ops-detection-remediation-19
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

command -v heimdall >/dev/null 2>&1 \
    || { echo "test-heimdall: heimdall is not on PATH" >&2; exit 1; }


TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "test-heimdall-restart.sh"

. "$HERE/testdb.sh"
if ! testdb_available; then
    printf 'SKIP test-heimdall: no bd engine available — the reconcile cases need a real bd.\n' \
        >&2
    printf '  embedded: install bd-embedded  server: testenv testdb\n' \
        >&2
    skip "no bd engine available"
fi
testdb_up heimdall_restart || { echo "test-heimdall: could not build a fixture database"; exit 1; }

. "$HERE/testlib/heimdall-fixture.sh"

echo
echo "heimdall.sh — restart loop detection via stub systemctl:"

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
    # SPIRA_DB via tl_config too: heimdall resolves it via cfg(), not the plain env prefix
    # below (same gap as heimdall()'s own call above it, line 78).
    tl_config SPIRA_RUN="$RUN" SPIRA_EXPORTER="" SPIRA_WIKI="" \
        SPIRA_HEIMDALL_RESTARTS=3 SPIRA_HEIMDALL_RESTART_WINDOW=3600 \
        SPIRA_DB="${HEIMDALL_DB:-$SPIRA_DB}"
    SPIRA_HOME="$SH" SPIRA_DB="${HEIMDALL_DB:-$SPIRA_DB}" \
    SPIRA_REPO="$TMP/repo" \
    SPIRA_SYSTEMCTL="$RESTART_SC" \
    SPIRA_HEIMDALL_SENTINEL_LOG="$RUN/sentinel.log" \
        command heimdall --home "$SH" "$@" 2>&1
}
restart_alert_status() {
    timeout 5 bd -C "$SPIRA_DB" list --all --limit 0 --label alert --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | python3 -c '
import sys, os, json
key = "restart-loop:spira-cockpit-prod.service"
try: d = json.load(sys.stdin)
except Exception: d = []
for i in (d if isinstance(d, list) else [d]):
    if "alert:" + key in (i.get("labels") or []):
        print("%s %s" % (i["id"], i.get("status"))); break
else:
    print("-")'
}

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
case "$_rst" in *" open") ok "restart: confirmed condition raises an open alert bead" ;;
    *) bad "restart raise" "expected an open bead, got [$_rst]" ;; esac
_rst_id="${_rst%% *}"

# Third pass on the same standing loop: no duplicate bead.
heimdall_restart >/dev/null
_rst_n="$(timeout 5 bd -C "$SPIRA_DB" list --all --limit 0 --label "alert:restart-loop:spira-cockpit-prod.service" --json 2>/dev/null \
    | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))')"
is "restart: a second pass on the same standing loop does not raise a duplicate" 1 "$_rst_n"

# Count goes DOWN (daemon-reload): no new alert; baseline resets.
printf '0\n' > "$TMP/restart-count"
heimdall_restart >/dev/null; heimdall_restart >/dev/null
[ "$(restart_alert_status)" = "$_rst_id closed" ] \
    && ok "restart: alert cleared after restart count went down" \
    || bad "restart reset" "expected [$_rst_id closed] got [$(restart_alert_status)]"

# Count rises again above threshold: alert reopened.
printf '8\n' > "$TMP/restart-count"
heimdall_restart >/dev/null; heimdall_restart >/dev/null
[ "$(restart_alert_status)" = "$_rst_id open" ] \
    && ok "restart: count rising again reopens the same bead" \
    || bad "restart reopen" "expected [$_rst_id open] got [$(restart_alert_status)]"

unset _rst _rst_id _rst_n

echo
echo "heimdall.sh — deliberately halted loop does not raise sentinel-stalled:"

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

testdb_drop >/dev/null 2>&1
tl_summary
