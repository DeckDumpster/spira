#!/usr/bin/env bash
#
# test-world-degraded.sh — world.sh start/status say DEGRADED rather than a bare RUNNING
#   when an essential timer is disabled with no recorded ctrl suspension.
#
# THE SCAR (sp-1ar8t, 2026-09-25). spira-sentinel-prod.timer and every other spira-*-prod
# timer were disabled — cause unestablished — and world.sh start skipped each one with a
# one-line "skipped <unit> (disabled)" that scrolled past under a `tail -3`, then printed
# a bare "spira: RUNNING". The fleet was dead for over an hour before anyone looked closely
# enough to find the per-unit skip lines. "The world is running" and "no part of the world
# is running" were the same output.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): every timer enabled must
# still produce a plain "spira: RUNNING" and exit 0 before the disabled case is believed.
#
# THE SUSPENDED CASE MUST NOT REGRESS (sp-cqyfy). A timer ctrl.sh has recorded a suspension
# for is a deliberate decision, not an accident — `start` must stay quiet and successful
# about it, exactly as before this bead.
#
# systemctl IS STUBBED. The stub answers is-enabled/is-active per timer base (stripping the
# instance suffix), so a fixture can disable exactly the timers a case cares about without
# reaching a real systemd user manager (law-gates-run-in-a-clean-environment).
#
# tier: T1
# covers: spira/world.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-world-degraded.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
SH="$TMP/spira"; mkdir -p "$SH" "$TMP/run"
cp "$HERE/world.sh" "$HERE/ctrl.sh" "$HERE/conf.sh" "$SH/"

CALLS="$TMP/sc-calls"
DISABLED_TIMERS=""   # comma-separated TIMER_PRIORITY bases (e.g. "spira-sentinel") to report disabled
EXTRA_TIMERS=""       # extra instance-qualified unit names "discovered" beyond TIMER_PRIORITY
export CALLS DISABLED_TIMERS EXTRA_TIMERS

write_sc() {
    cat > "$TMP/systemctl" <<'SC'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CALLS"
verb=""; subj=""
for a; do
    case "$a" in --user|--no-legend|--all|-p|--value|--state=active) continue ;; esac
    if [ -z "$verb" ]; then verb="$a"; elif [ -z "$subj" ]; then subj="$a"; fi
done
base="${subj%.timer}"; base="${base%.service}"; base="${base%-prod}"
is_dis=0
case ",${DISABLED_TIMERS:-}," in *",${base},"*) is_dis=1 ;; esac
case "$verb" in
    is-enabled)
        if [ "$is_dis" = 1 ]; then echo disabled; exit 1; else echo enabled; exit 0; fi ;;
    is-active)
        # Every named unit "exists" and is active regardless of enablement — this stub
        # only needs to make world.sh's discovery resolve the instance-qualified form.
        echo active; exit 0 ;;
    show) echo success ;;
    list-unit-files|list-units)
        # Only the plain timer-discovery glob returns the extra fixture timers — the
        # watcher-revival and mail-deliver queries use different literal patterns and
        # must see none of them.
        [ "$subj" = 'spira-*.timer' ] && [ -n "${EXTRA_TIMERS:-}" ] && printf '%s\n' $EXTRA_TIMERS
        exit 0 ;;
    start|stop) exit 0 ;;
    *) exit 0 ;;
esac
SC
    chmod +x "$TMP/systemctl"
}
write_sc

run() {   # run <world.sh args...> -> sets $out and $rc
    : > "$CALLS"
    rc=0
    out="$(PATH="$SH:$PATH" SPIRA_HOME="$SH" SPIRA_PROD="$SH" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no.conf" \
           SPIRA_DB="$TMP/no-db" SPIRA_SYSTEMCTL="$TMP/systemctl" \
           SPIRA_CTRL="$TMP/ctrl.json" SPIRA_INSTANCE=prod \
           bash "$SH/world.sh" "$@" 2>&1)" || rc=$?
}

# ============================================================================
echo
echo "positive control — every essential timer enabled: plain RUNNING, exit 0:"
# ============================================================================
DISABLED_TIMERS=""; rm -f "$TMP/ctrl.json"
run start
is     "start exits 0 when nothing is disabled" "0" "$rc"
want   "prints a plain RUNNING"                 "spira: RUNNING" "$out"
nowant "does not say DEGRADED"                  "DEGRADED"       "$out"

run status
nowant "status does not say DEGRADED either" "DEGRADED" "$out"

# ============================================================================
echo
echo "disabled essential timer, no ctrl suspension — DEGRADED and non-zero:"
# ============================================================================
DISABLED_TIMERS="spira-sentinel"; rm -f "$TMP/ctrl.json"
run start
is     "start exits non-zero" "1" "$rc"
want   "says DEGRADED"                                "DEGRADED"                    "$out"
want   "names the disabled timer"                     "spira-sentinel-prod.timer"   "$out"
want   "still reports the plain skip line"            "skipped spira-sentinel-prod.timer (disabled)" "$out"

run status
want "status also reports DEGRADED for the same condition" "DEGRADED" "$out"
want "status names the disabled timer"                     "spira-sentinel-prod.timer" "$out"

# ============================================================================
echo
echo "ctrl-suspended disabled timer — quiet and successful (sp-cqyfy must not regress):"
# ============================================================================
DISABLED_TIMERS="spira-sentinel"
python3 - "$TMP/ctrl.json" <<'PY'
import json, sys
json.dump({"spira-sentinel": {"suspend": {"reason": "stopped for maintenance", "owner": "sp-x"}}},
          open(sys.argv[1], "w"))
PY
run start
is     "start exits 0 when the disabled timer is ctrl-suspended" "0" "$rc"
want   "prints a plain RUNNING"                                  "spira: RUNNING" "$out"
nowant "does not say DEGRADED for a recorded decision"           "DEGRADED"       "$out"
want   "reports the suspension, not a bare skip"                 "suspended: stopped for maintenance" "$out"

run status
nowant "status does not say DEGRADED for a recorded decision" "DEGRADED" "$out"
rm -f "$TMP/ctrl.json"

# ============================================================================
echo
echo "a disabled non-essential timer does not DEGRADE anything:"
# ============================================================================
DISABLED_TIMERS="spira-groom"; EXTRA_TIMERS="spira-groom-prod.timer"; rm -f "$TMP/ctrl.json"
run start
is     "start still exits 0 — spira-groom is not essential" "0" "$rc"
want   "prints a plain RUNNING"                              "spira: RUNNING" "$out"
want   "spira-groom is still reported skipped"               "skipped spira-groom-prod.timer (disabled)" "$out"
nowant "does not say DEGRADED for a non-essential timer"     "DEGRADED"       "$out"
DISABLED_TIMERS=""; EXTRA_TIMERS=""

tl_summary
