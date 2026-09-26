#!/usr/bin/env bash
#
# test-world.sh — world.sh stops work services and reports them honestly.
#
#   ./test-world.sh
#
# WHAT THIS SUITE COVERS
# ----------------------
# 2026-09-07: world.sh stop printed STOPPED while spira-landing.service was still running.
# The check did not catch it because world.sh's own status reported timers and live aeons,
# and spira-landing is neither — it is a transient systemd unit that executes the same
# gate.sh passes an aeon does, supervised outside the timer loop.
#
# PROPERTIES KEPT HERE, each a pair (law-absence-needs-a-positive-control). The service and
# timer FILTERS themselves (work_services, _is_ci_watcher, _status_timer_row, _start_action)
# are T1 decision tables in test-world-decide.sh now; this file keeps only what those tables
# do not reach — full-process wiring and /proc.
#
#   1. status reports spira-landing.service as active when it is, and inactive when it is not.
#      A status that always shows "inactive" and one that is merely correct look identical.
#
#   2. stop exits non-zero and does NOT print STOPPED when a service cannot be stopped.
#      Without this, the scar is invisible: the operator halts, sees the success message,
#      and the worker goes on running.
#
#   3. live_workers (/proc) is non-zero when a process matching gate.sh or landing.sh is
#      running, and zero when it is not.
#
#   4. the status line's own CI-watcher-listing loop (over CI_WATCHER_BASES, not through
#      _is_ci_watcher) names the watchers that are active and says so when none are.
#
# systemctl IS STUBBED, not reached. A suite that asks the real systemd is green for as long
# as the box happens to be in the state its author had (law-gates-run-in-a-clean-environment).
# The stub records every call world.sh makes, so the assertions are about what world.sh DID,
# not about what systemd reported.
#
# defect: sp-i96t
# covers: spira/world.sh
# hermetic-ok: stubs systemctl via SPIRA_SYSTEMCTL; /proc scan uses real background process
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-world.sh"

# set -m gives each background job its own process group (PGID = job PID), so
# kill -- -$WORKER_PID reaches both the bash wrapper and any child (e.g. sleep)
# it spawned — leaving no orphans in the caller's process group on cleanup.
set -m
TMP="$(mktemp -d)"; trap 'kill -- -"$WORKER_PID" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
WORKER_PID=""

SH="$TMP/spira"
RUN="$TMP/run"
CALLS="$TMP/sc-calls"   # every systemctl call world.sh makes, one per line
export CALLS            # the stub appends to this path; it must be visible in its environment
mkdir -p "$SH" "$RUN"

# A copy of the harness the world.sh under test can source without touching the real box.
cp "$HERE/world.sh" "$HERE/conf.sh" "$SH/"
# slay.sh is called by `stop` for live aeons; stub it so no real aeons are touched.
printf '#!/usr/bin/env bash\nexit 0\n' > "$SH/slay.sh"; chmod +x "$SH/slay.sh"

# The systemctl stub. It records every call; what it answers depends on the service asked for.
# ACTIVE_SVC controls which service is "active" for the current test.
ACTIVE_SVC=""
write_sc() {
    cat > "$TMP/systemctl" <<'SC'
#!/usr/bin/env bash
# Record this call (without the --user flag, which is noise in assertions).
printf '%s\n' "$*" >> "$CALLS"
cmd=""
svc=""
for a; do
    case "$a" in --user|--state=active|--no-legend|--all) ;; *) [ -z "$cmd" ] && cmd="$a" || svc="$a" ;; esac
done
case "$cmd" in
is-active)
    if [ "$svc" = "$ACTIVE_SVC" ]; then echo active; exit 0; fi
    IFS=, read -ra _ats <<< "${ACTIVE_TIMERS:-}"
    for _at in "${_ats[@]}"; do [ "$svc" = "$_at" ] && { echo active; exit 0; }; done
    echo inactive; exit 3 ;;
list-units)
    case "${svc:-}" in
    *service*)
        [ -n "$ACTIVE_SVC" ] && printf '%s active running\n' "$ACTIVE_SVC" ;;
    *timer*)
        IFS=, read -ra _ats <<< "${ACTIVE_TIMERS:-}"
        for _at in "${_ats[@]}"; do printf '%s active running\n' "$_at"; done ;;
    *)
        [ -n "$ACTIVE_SVC" ] && printf '%s active running\n' "$ACTIVE_SVC"
        IFS=, read -ra _ats <<< "${ACTIVE_TIMERS:-}"
        for _at in "${_ats[@]}"; do printf '%s active running\n' "$_at"; done ;;
    esac
    exit 0 ;;
list-unit-files)
    IFS=, read -ra _ats <<< "${ACTIVE_TIMERS:-}"
    for _at in "${_ats[@]}"; do printf '%s enabled\n' "$_at"; done
    exit 0 ;;
stop)
    if [ "$svc" = "$STOP_FAILS" ]; then exit 1
    else exit 0
    fi ;;
*)  exit 0 ;;
esac
SC
    chmod +x "$TMP/systemctl"
    export ACTIVE_SVC STOP_FAILS ACTIVE_TIMERS
}
STOP_FAILS=""
ACTIVE_TIMERS=""

world() {
    : > "$CALLS"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_DB="$TMP/no-db" \
    SPIRA_SYSTEMCTL="$TMP/systemctl" \
        bash "$SH/world.sh" "$@" 2>&1
}
world_rc() {
    : > "$CALLS"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_DB="$TMP/no-db" \
    SPIRA_SYSTEMCTL="$TMP/systemctl" \
        bash "$SH/world.sh" "$@" 2>&1; echo "$?"
}

# --------------------------------------------------------------------------------------
# 1. STATUS REPORTS SPIRA-LANDING.SERVICE
# The positive control comes first: a status that always says inactive looks exactly like
# one that is merely correct when nothing is running (law-absence-needs-a-positive-control).
# --------------------------------------------------------------------------------------
echo
echo "status reports spira-landing.service:"

ACTIVE_SVC=spira-landing.service; write_sc
out="$(world status)"
want "when active, status shows it active" "spira-landing.service" "$out"
want "and shows the state"                 "active"                  "$out"

ACTIVE_SVC=""; write_sc
out="$(world status)"
want  "when inactive, spira-landing.service still appears" "spira-landing.service" "$out"
want  "and is shown as inactive"                           "inactive"              "$out"

# Which services `stop` acts on (work_services' filter) and which timers a plain halt
# spares (_is_ci_watcher) are T1 decision tables now: spira/test-world-decide.sh.

# --------------------------------------------------------------------------------------
# 2. STOP EXITS NON-ZERO WHEN A SERVICE CANNOT BE STOPPED
# The scar: world.sh printed STOPPED while spira-landing.service was still running.
# A halt that cannot stop something must say so and exit non-zero.
# --------------------------------------------------------------------------------------
echo
echo "stop exits non-zero when a service cannot be stopped:"

ACTIVE_SVC=spira-landing.service; STOP_FAILS=spira-landing.service; write_sc
rc_out="$(world_rc stop)"
rc="${rc_out##*$'\n'}"; rc="${rc%%[!0-9]*}"
out="${rc_out%$'\n'*}"
is     "exit code is non-zero"                    "1"      "$rc"
nowant "and does not print STOPPED"               "STOPPED" "$out"
want   "and warns about the failure"              "WARNING"  "$out"
STOP_FAILS=""

# --------------------------------------------------------------------------------------
# 3. LIVE WORKERS (/proc) — status counts running gate.sh / landing.sh processes
#
# A REAL background process, because /proc is the real thing and cannot be stubbed. The
# process is started under $SPIRA_HOME so world.sh's own live_workers() scan finds it —
# the scan is keyed on $SPIRA_HOME/gate.sh and $SPIRA_HOME/landing.sh in the cmdline.
# --------------------------------------------------------------------------------------
echo
echo "live workers (/proc) scan:"

# The positive control: a process that IS running must appear in the count.
# We start a background bash that names $SH/landing.sh as its argv[0] equivalent:
# argv: bash <path>/landing.sh
FAKE_LANDING="$SH/landing.sh"
printf '#!/usr/bin/env bash\nsleep 30\n' > "$FAKE_LANDING"; chmod +x "$FAKE_LANDING"

ACTIVE_SVC=""; write_sc
bash "$FAKE_LANDING" &
WORKER_PID=$!

out="$(world status)"
want "status reports a live worker when one is running" "live workers (/proc)" "$out"
wcount="$(printf '%s' "$out" | grep 'live workers' | grep -oE '[0-9]+' | tail -1)"
[ "${wcount:-0}" -ge 1 ] && ok "live workers count is at least 1" \
                           || bad "live workers count" "wanted >=1, got [${wcount:-?}]"

kill -- -"$WORKER_PID" 2>/dev/null; wait "$WORKER_PID" 2>/dev/null; WORKER_PID=""

out="$(world status)"
wcount="$(printf '%s' "$out" | grep 'live workers' | grep -oE '[0-9]+' | tail -1)"
is "and is 0 after the process exits" "0" "${wcount:-?}"

# ---- DRAIN / RESUME -----------------------------------------------------------------
# The property that matters is what drain does NOT do. The first implementation stopped
# spira-sentinel.timer to halt summons, which also halted LANDING — landing is a leg of the
# sentinel pass, not a timer — and three finished branches sat unlanded (2026-09-08). So the
# assertion is negative and deliberate: drain must touch no unit at all.

out="$(world drain)"
want "drain reports DRAINED with an empty pool" "DRAINED" "$out"
want "drain says the loop keeps running" "landing and reaping continue" "$out"

# THE LOAD-BEARING ONE. If drain ever stops a unit again, this fails.
calls="$(cat "$CALLS" 2>/dev/null || true)"
nowant "drain stops no timer" "stop spira-sentinel.timer" "$calls"
nowant "drain stops no landing service" "stop spira-landing.service" "$calls"

[ -f "$RUN/world.draining" ] && ok "drain writes the stamp" \
                             || bad "drain writes the stamp" "no $RUN/world.draining"
want "the stamp says how to lift it" "resume" "$(cat "$RUN/world.draining" 2>/dev/null)"

out="$(world status)"
want "status reports DRAINING while the stamp exists" "DRAINING" "$out"

out="$(world resume)"
want "resume reports it" "summons resumed" "$out"
[ -f "$RUN/world.draining" ] && bad "resume removes the stamp" "stamp still present" \
                             || ok "resume removes the stamp"

out="$(world status)"
nowant "status stops saying DRAINING after resume" "DRAINING" "$out"

out="$(world resume)"
want "resume on a world that was not draining says so" "not draining" "$out"

# drain --timeout 0 with a live aeon (NOT DRAINED / REMAIN GATED / exit 1) is a near-verbatim
# duplicate of test-world-drain-deadline.sh case 3 (cluster 4, docs/test-plan/instance-lifecycle.md):
# kept there only.

# ---- THE GATE MUST COVER EVERY DOOR, NOT JUST THE TIDY ONE --------------------------
# summon_fayth() is called only from sentinel.sh, and that grep is what made the first
# version of this gate look complete. It was not: spira-ops.service and spira-qa.service
# ExecStart aeon.sh DIRECTLY, so ops and qa never reach summon_fayth. A qa aeon was summoned
# four minutes into a drain that had reported DRAINED (sp-637b, 2026-09-08).
#
# So this asserts on the FILE, not on behaviour: every unit template whose ExecStart is
# aeon.sh is a door, and aeon.sh itself must carry the gate. A new ops-shaped persona added
# later fails here rather than in production.
HARNESS="$(cd "$HERE/.." && pwd)"
if [ -d "$HARNESS/systemd" ]; then
    doors="$(grep -l 'ExecStart=.*aeon\.sh' "$HARNESS/systemd"/*.service 2>/dev/null | wc -l)"
    [ "${doors:-0}" -ge 1 ] && ok "units that ExecStart aeon.sh directly exist ($doors) — the gate must cover them" \
                            || bad "direct-ExecStart doors" "expected at least one, found ${doors:-0}"
    grep -q 'world.draining' "$HARNESS/spira/aeon.sh" \
        && ok "aeon.sh itself carries the drain gate" \
        || bad "aeon.sh carries the drain gate" "no world.draining check in aeon.sh"
    grep -q 'world.draining' "$HARNESS/spira/lib.sh" \
        && ok "summon_fayth also carries it (cheaper: never starts the unit)" \
        || bad "summon_fayth carries the drain gate" "no world.draining check in lib.sh"
    grep -q 'world.halted' "$HARNESS/spira/aeon.sh" \
        && ok "aeon.sh carries the halt gate" \
        || bad "aeon.sh carries the halt gate" "no world.halted check in aeon.sh"
    grep -q 'world.halted' "$HARNESS/spira/lib.sh" \
        && ok "summon_fayth carries the halt gate" \
        || bad "summon_fayth carries the halt gate" "no world.halted check in lib.sh"
fi

# --------------------------------------------------------------------------------------
# 4. STATUS NAMES THE ACTIVE CI WATCHERS
# _is_ci_watcher itself (whether a timer counts, and that a plain halt spares it while
# --hard does not) is the T1 table in test-world-decide.sh now. What is not extracted, and
# stays here, is the status line's OWN loop over CI_WATCHER_BASES that formats the message.
# --------------------------------------------------------------------------------------
echo
echo "status names the active CI watchers:"

ACTIVE_TIMERS="spira-gate-check.timer,spira-pr-notify.timer"
ACTIVE_SVC=""; write_sc
world stop >/dev/null   # HALTED state is what gates the status line below

out="$(world status)"
want "status names gate-check as CI watcher" "spira-gate-check" "$out"
want "status names pr-notify as CI watcher"  "spira-pr-notify"  "$out"

# Status with watchers inactive: nobody is watching CI.
ACTIVE_TIMERS=""; write_sc
out="$(world status)"
want "status says nobody is watching CI when watchers are inactive" "nobody" "$out"
ACTIVE_TIMERS=""

tl_summary
