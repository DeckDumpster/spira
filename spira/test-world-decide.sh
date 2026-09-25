#!/usr/bin/env bash
#
# test-world-decide.sh — T1 decision tables for world.sh start/stop/status.
#
# Four pure functions, each tested directly against a stubbed $SC or a real (temp) ctrl file
# rather than by spawning world.sh once per case:
#
#   ctrl_is_suspended / ctrl_load_suspended (spira/ctrl.sh)  the single-read control-plane
#     predicate shared by world.sh start and systemd/install.sh's apply loop (cluster 6,
#     docs/test-plan/instance-lifecycle.md). MERGES the coverage of the now-deleted
#     test-world-start-honours-ctrl.sh's ctrl half.
#
#   _start_action <timer> <is-enabled>  the start/skip decision (UC-instance-lifecycle-42).
#     MERGES test-world-start-honours-ctrl.sh in full: its "suspension beats is-enabled",
#     "disabled skipped", and both positive controls are table rows here.
#
#   work_services()  the service filter `stop` uses (UC-instance-lifecycle-41): work units
#     included, cockpit/loom/watch@ excluded in both bare and instance-qualified form.
#
#   _status_timer_row <timer>  the row `status` prints per timer (UC-instance-lifecycle-44):
#     plain state, or "(svc: <result>)" appended when the paired service's last run failed.
#
#   _is_ci_watcher <timer>  which timers `stop` spares on a plain halt (part of UC-41/UC-44).
#
# Loaded via WORLD_LIB=1 (section 5, docs/test-plan/instance-lifecycle.md): sourcing world.sh
# this way defines the functions above without discovering TIMERS or spawning systemctl, so
# every case below is one function call, not one world.sh process.
#
# tier: T1
# covers: spira/world.sh spira/ctrl.sh UC-instance-lifecycle-41 UC-instance-lifecycle-42 UC-instance-lifecycle-44 UC-instance-lifecycle-45
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
SH="$TMP/spira"; mkdir -p "$SH" "$TMP/run"
cp "$HERE/world.sh" "$HERE/ctrl.sh" "$HERE/conf.sh" "$SH/"

export SPIRA_HOME="$SH" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no.conf" SPIRA_DB="$TMP/no-db"
export SPIRA_CTRL="$TMP/ctrl.json"
export SPIRA_INSTANCE="prod"

# A stub $SC, reconfigured per section via these env vars, and recording every call.
CALLS="$TMP/sc-calls"; export CALLS
SVC_LIST=""       # for `list-units spira-*.service --state=active`: newline-separated names
SVC_RESULT=""     # for `show <svc> -p Result --value`
TIMER_ACTIVE=""   # for `is-active <timer>`: the exact unit that answers "active"
export SVC_LIST SVC_RESULT TIMER_ACTIVE
write_sc() {
    cat > "$TMP/sc" <<'SC'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CALLS"
verb=""; subj=""; extra=()
for a; do
    case "$a" in
        --user|--no-legend|--state=active|--all|-p|--value) [ "$a" = "-p" ] && extra=(-p) ;;
        *) if [ -z "$verb" ]; then verb="$a"; else subj="$a"; fi ;;
    esac
done
case "$verb" in
    is-active)
        [ -n "$TIMER_ACTIVE" ] && [ "$subj" = "$TIMER_ACTIVE" ] && { echo active; exit 0; }
        echo inactive; exit 3 ;;
    show)
        printf '%s\n' "$SVC_RESULT" ;;
    list-units)
        [ -n "$SVC_LIST" ] && printf '%s\n' "$SVC_LIST" | sed 's/$/ active running -/'
        exit 0 ;;
    *) exit 0 ;;
esac
SC
    chmod +x "$TMP/sc"
}
write_sc
export SPIRA_SYSTEMCTL="$TMP/sc"

world_lib() {   # world_lib <bash-fragment> -> load world.sh's functions, run the fragment
    bash -c 'WORLD_LIB=1 . "$SPIRA_HOME/world.sh"; '"$1" 2>&1
}

# ============================================================================
echo "ctrl_is_suspended / ctrl_load_suspended — the shared control-plane predicate:"
# ============================================================================
rm -f "$SPIRA_CTRL"
out="$(bash -c 'CTRL_LIB=1 . "$SPIRA_HOME/ctrl.sh"; declare -A A=(); ctrl_load_suspended A; echo "n=${#A[@]}"')"
is "no control file -> zero suspended entries" "n=0" "$out"

python3 - "$SPIRA_CTRL" <<'PY'
import json, sys
json.dump({"spira-suites": {"suspend": {"reason": "testing sp-rnps9", "owner": "sp-x"}}}, open(sys.argv[1], "w"))
PY

out="$(bash -c 'CTRL_LIB=1 . "$SPIRA_HOME/ctrl.sh"; declare -A A=(); ctrl_load_suspended A
    if r=$(ctrl_is_suspended A spira-suites); then echo "suspended:$r"; else echo "not-suspended"; fi')"
is "declared-suspended subject reports its reason" "suspended:testing sp-rnps9" "$out"

out="$(bash -c 'CTRL_LIB=1 . "$SPIRA_HOME/ctrl.sh"; declare -A A=(); ctrl_load_suspended A
    if r=$(ctrl_is_suspended A spira-other); then echo "suspended:$r"; else echo "not-suspended"; fi')"
is "positive control — a different subject is NOT reported suspended" "not-suspended" "$out"

# ============================================================================
echo
echo "_start_action <timer> <is-enabled> — the world.sh start decision table:"
# ============================================================================
# Table rows, each (suspended?, is-enabled) -> expected action. Suspension beats is-enabled
# (sp-cqyfy: start must not silently reverse an operator decision), which is why the
# suspended+enabled and suspended+disabled rows both skip.
run_start_action() {   # run_start_action <timer> <is-enabled> -> action string
    bash -c '
        CTRL_LIB=1 . "$SPIRA_HOME/ctrl.sh"
        declare -A CTRL_SUSPENDED=(); ctrl_load_suspended CTRL_SUSPENDED
        WORLD_LIB=1 . "$SPIRA_HOME/world.sh"
        _start_action "$1" "$2"' _ "$1" "$2"
}

rm -f "$SPIRA_CTRL"
is "not suspended + enabled -> start"           "start"                        "$(run_start_action spira-testsubject-prod.timer enabled)"
is "not suspended + disabled -> skip-disabled"  "skip-disabled"                "$(run_start_action spira-testsubject-prod.timer disabled)"

python3 - "$SPIRA_CTRL" <<'PY'
import json, sys
json.dump({"spira-testsubject": {"suspend": {"reason": "stopped for sp-cqyfy test", "owner": "sp-cqyfy"}}}, open(sys.argv[1], "w"))
PY
is "suspended + enabled -> skip-suspended (suspension wins)"  "skip-suspended stopped for sp-cqyfy test" \
    "$(run_start_action spira-testsubject-prod.timer enabled)"
is "suspended + disabled -> skip-suspended (suspension still wins)" "skip-suspended stopped for sp-cqyfy test" \
    "$(run_start_action spira-testsubject-prod.timer disabled)"

# Positive control: lift the suspension -> the same timer goes back to a plain decision.
rm -f "$SPIRA_CTRL"
is "positive control — after resume, the same timer decides on is-enabled alone" "start" \
    "$(run_start_action spira-testsubject-prod.timer enabled)"

# Instance-suffix stripping: the ctrl subject is spira-testsubject, not spira-testsubject-prod.
python3 - "$SPIRA_CTRL" <<'PY'
import json, sys
json.dump({"spira-testsubject": {"suspend": {"reason": "r", "owner": "sp-x"}}}, open(sys.argv[1], "w"))
PY
is "instance suffix is stripped before the ctrl lookup" "skip-suspended r" \
    "$(run_start_action spira-testsubject-prod.timer enabled)"
rm -f "$SPIRA_CTRL"

# ============================================================================
echo
echo "work_services() — the service filter world.sh stop uses:"
# ============================================================================
SVC_LIST="$(printf '%s\n' spira-sentinel-prod.service spira-landing.service \
    spira-cockpit-prod.service spira-loom-prod.service spira-cockpit.service \
    spira-loom.service spira-watch@x.service)"
write_sc

out="$(world_lib 'work_services')"
want   "sentinel timer's service is included" "spira-sentinel-prod.service" "$out"
want   "landing is included"                  "spira-landing.service"      "$out"
nowant "instance-qualified cockpit is excluded" "spira-cockpit-prod.service" "$out"
nowant "instance-qualified loom is excluded"    "spira-loom-prod.service"    "$out"
nowant "bare cockpit is excluded"               "spira-cockpit.service"      "$out"
nowant "bare loom is excluded"                  "spira-loom.service"         "$out"
nowant "watch@ services are excluded (handled by --hard separately)" "spira-watch@x.service" "$out"

SVC_LIST=""; write_sc

# ============================================================================
echo
echo "_status_timer_row <timer> — row formatting world.sh status uses:"
# ============================================================================
TIMER_ACTIVE="spira-sentinel-prod.timer"; SVC_RESULT="success"; write_sc
out="$(world_lib '_status_timer_row spira-sentinel-prod.timer')"
want   "active timer with a successful last run shows its state" "active" "$out"
nowant "a successful run adds no svc suffix"                     "(svc:"  "$out"

TIMER_ACTIVE=""; SVC_RESULT="success"; write_sc
out="$(world_lib '_status_timer_row spira-sentinel-prod.timer')"
want "an inactive timer shows inactive" "inactive" "$out"

TIMER_ACTIVE=""; SVC_RESULT="exit-code"; write_sc
out="$(world_lib '_status_timer_row spira-sentinel-prod.timer')"
want "a failed last run appends the svc result" "(svc: exit-code)" "$out"

TIMER_ACTIVE=""; SVC_RESULT=""; write_sc
out="$(world_lib '_status_timer_row spira-sentinel-prod.timer')"
nowant "no result (unit never ran) adds no svc suffix" "(svc:" "$out"

# ============================================================================
echo
echo "_is_ci_watcher <timer> — which timers a plain halt spares:"
# ============================================================================
want   "bare gate-check name is a CI watcher"       "yes" "$(world_lib '_is_ci_watcher spira-gate-check.timer && echo yes || echo no')"
want   "instance-qualified pr-notify is a CI watcher" "yes" "$(world_lib '_is_ci_watcher spira-pr-notify-prod.timer && echo yes || echo no')"
want   "positive control — an unrelated timer is not a CI watcher" "no" "$(world_lib '_is_ci_watcher spira-sentinel-prod.timer && echo yes || echo no')"

tl_summary
