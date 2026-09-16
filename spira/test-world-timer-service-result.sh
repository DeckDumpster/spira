#!/usr/bin/env bash
#
# test-world-timer-service-result.sh — status annotates a timer row with its service result.
#
#   ./test-world-timer-service-result.sh
#
# WHAT THIS SUITE COVERS
# ----------------------
# sp-2z9y: world.sh status reported a timer as active while its service had failed 12/12
# times. A timer that fires faithfully into a service that always fails reads as healthy.
#
# TWO PROPERTIES (law-absence-needs-a-positive-control):
#
#   1. When the last service run did NOT succeed (Result=timeout, exit-code, etc.),
#      status annotates the timer row with the service result, so the failure is visible.
#
#   2. When the last service run succeeded (Result=success or no result recorded),
#      status shows only the timer state — no spurious annotations on healthy units.
#
# systemctl IS STUBBED. The show command is the critical new seam; the stub records every
# call so assertions are on what world.sh DID, not on what systemd reported.
#
# defect: sp-2z9y
# covers: spira/world.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-world-timer-service-result.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

SH="$TMP/spira"
RUN="$TMP/run"
CALLS="$TMP/sc-calls"
export CALLS
mkdir -p "$SH" "$RUN"

cp "$HERE/world.sh" "$HERE/conf.sh" "$SH/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$SH/slay.sh"; chmod +x "$SH/slay.sh"

# write_sc ACTIVE_TIMER SVC_RESULT
#   ACTIVE_TIMER — timer unit that is-active returns "active" for
#   SVC_RESULT   — what "show <svc> -p Result --value" returns for that timer's service
#                  (empty string means the service has no recorded result yet)
write_sc() {
    local active_timer="${1:-}" svc_result="${2:-}"
    cat > "$TMP/systemctl" <<SC
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "\$CALLS"
cmd=""; unit=""
for a; do
    case "\$a" in --user|--state=active|--no-legend|--all|-p|Result|--value) ;;
    *) [ -z "\$cmd" ] && cmd="\$a" || unit="\$a" ;;
    esac
done
case "\$cmd" in
is-active)
    [ "\$unit" = "$active_timer" ] && { echo active; exit 0; } || { echo inactive; exit 3; } ;;
is-enabled) exit 1 ;;
list-units)
    [ -n "$active_timer" ] && printf '%s active running\n' "$active_timer"
    exit 0 ;;
list-unit-files) exit 0 ;;
show)
    printf '%s\n' "$svc_result"
    exit 0 ;;
stop) exit 0 ;;
*)   exit 0 ;;
esac
SC
    chmod +x "$TMP/systemctl"
}

world_status() {
    : > "$CALLS"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_DB="$TMP/no-db" \
    SPIRA_SYSTEMCTL="$TMP/systemctl" \
        bash "$SH/world.sh" status 2>&1
}

# --------------------------------------------------------------------------------------
# 1. TIMER WITH A FAILED SERVICE RESULT — annotation must appear
#
# The positive control: a status that never shows annotations is indistinguishable from
# one that correctly suppresses them when the service is healthy. The failed case must
# visibly annotate the timer row (law-absence-needs-a-positive-control).
# --------------------------------------------------------------------------------------
echo
echo "timer with last service result = timeout:"

write_sc "spira-watchtower-prod.timer" "timeout"
out="$(world_status)"
want "timer row shows the timer name"        "spira-watchtower-prod.timer" "$out"
want "timer row shows 'active'"              "active"                       "$out"
want "timer row annotates the svc result"    "svc: timeout"                 "$out"
want "show was called for the service"       "show spira-watchtower-prod.service" "$(cat "$CALLS")"

echo
echo "timer with last service result = exit-code:"

write_sc "spira-sentinel-prod.timer" "exit-code"
out="$(world_status)"
want "exit-code result annotated"    "svc: exit-code"                "$(printf '%s' "$out" | grep spira-sentinel-prod)"
want "show called for sentinel svc"  "show spira-sentinel-prod.service" "$(cat "$CALLS")"

# --------------------------------------------------------------------------------------
# 2. TIMER WITH A SUCCESSFUL SERVICE RESULT — no annotation
#
# A green timer must not show any "svc:" annotation. A spurious annotation on every
# healthy row trains the operator to ignore it on the day it matters.
# --------------------------------------------------------------------------------------
echo
echo "timer with last service result = success (no annotation):"

write_sc "spira-watchtower-prod.timer" "success"
out="$(world_status)"
want   "timer row still shows 'active'"          "active"     "$out"
nowant "no annotation on a successful service"   "svc:"       "$out"

echo
echo "timer with no recorded service result (no annotation):"

write_sc "spira-watchtower-prod.timer" ""
out="$(world_status)"
want   "timer still appears"                     "spira-watchtower-prod.timer" "$out"
nowant "no annotation when result is empty"      "svc:"                        "$out"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
