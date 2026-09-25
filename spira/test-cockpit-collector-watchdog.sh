#!/usr/bin/env bash
#
# test-cockpit-collector-watchdog.sh — the staleness watchdog in layout.sh restarts
# the collector when it predates cockpit.sh (SPIRA_PROD), and leaves it alone when
# the source is older than the process.
#
# THREE PROPERTIES, each requiring a positive control (a run that does nothing passes
# identically whether the logic is correct or the watchdog is aimed at a dead unit):
#
#   1. STALE COLLECTOR: process started before cockpit.sh was promoted → restart fired.
#      This is the positive control. Without it, a watchdog that never fires is
#      indistinguishable from one that correctly finds nothing to do.
#
#   2. FRESH COLLECTOR: process started after cockpit.sh → no restart (no false positives).
#
#   3. UNIT DISCOVERY: only the instance-qualified unit (spira-cockpit-prod.service) is
#      active; the plain unit (spira-cockpit.service) is inactive. The watchdog finds and
#      uses the active unit. This is the first bug this suite closes: the old code had a
#      literal spira-cockpit.service, which is inactive on every migrated box.
#
# The watchdog function is extracted from layout.sh at runtime — not copied — so the suite
# stays in sync when the function changes.
#
# defect: sp-vjiug
# covers: cockpit/layout.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT="$(dirname "$HERE")/cockpit"
LAYOUT="$COCKPIT/layout.sh"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
BIN="$TMP/bin"
PROD="$TMP/prod"
mkdir -p "$BIN" "$PROD"
RESTART_LOG="$TMP/restart.log"
HEAL_LOG="$TMP/heal.log"

cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

echo "test-cockpit-collector-watchdog.sh"

# Extract the watchdog function from layout.sh. The function has no nested function
# definitions, so the first bare '}' (no leading whitespace) closes it.
FUNC_BODY=$(awk '/^restart_spira_collector_if_stale\(\)/{p=1} p{print} p && /^\}$/{exit}' "$LAYOUT")
if [ -z "$FUNC_BODY" ]; then
    echo "SKIP: restart_spira_collector_if_stale not found in layout.sh" >&2
    exit 0
fi

# ── Permanent mock commands ────────────────────────────────────────────────────
# systemctl reads MOCK_ACTIVE_SFX and MOCK_RESTART_LOG from the environment.
# The unit name is always the first positional arg after the sub-command; the
# 'show' call places it before '-p MainPID --value' so $1 is always the unit.
cat > "$BIN/systemctl" <<'SH'
#!/usr/bin/env bash
shift          # drop --user
sub="$1"; shift
unit="$1"     # first remaining arg is always the unit name
sfx="${MOCK_ACTIVE_SFX:-prod}"
case "$sub" in
    cat)
        [[ "$unit" == *"$sfx"* ]] && exit 0 || exit 1 ;;
    is-active)
        [[ "$unit" == *"$sfx"* ]] && echo "active" || echo "inactive"
        exit 0 ;;
    show)
        # -p MainPID --value: return a fake PID for the active unit only
        [[ "$unit" == *"$sfx"* ]] && echo "99999" || echo "0"
        exit 0 ;;
    restart)
        printf '%s\n' "$unit" >> "${MOCK_RESTART_LOG:-/dev/null}"
        exit 0 ;;
esac
SH
chmod +x "$BIN/systemctl"

# ps returns MOCK_PS_ELAPSED seconds for any PID (proc_start subtracts from now).
cat > "$BIN/ps" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "${MOCK_PS_ELAPSED:-0}"
SH
chmod +x "$BIN/ps"

# ── Test runner ────────────────────────────────────────────────────────────────
# run_watchdog <elapsed_s> <src_age_s> [instance] [active_sfx]
#   elapsed_s  : how many seconds ago the fake process started
#   src_age_s  : how old cockpit.sh is (seconds before now; 0 = just updated)
#   instance   : SPIRA_INSTANCE value (default: prod)
#   active_sfx : substring that makes the mock systemctl report a unit as active
# Prints the contents of the restart log (empty when no restart was requested).
run_watchdog() {
    local elapsed_s="$1" src_age_s="$2" instance="${3:-prod}" active_sfx="${4:-prod}"
    rm -f "$RESTART_LOG" "$HEAL_LOG"

    # Set cockpit.sh mtime to src_age_s seconds ago.
    local now; now=$(date +%s)
    touch -d "@$(( now - src_age_s ))" "$PROD/cockpit.sh"

    PATH="$BIN:$PATH" \
    SPIRA_INSTANCE="$instance" \
    SPIRA_PROD="$PROD" \
    MOCK_ACTIVE_SFX="$active_sfx" \
    MOCK_RESTART_LOG="$RESTART_LOG" \
    MOCK_PS_ELAPSED="$elapsed_s" \
    bash <<DRIVER
heal_log() { printf '%s %s\n' "\$(date '+%Y-%m-%dT%H:%M:%S')" "\$*" >> "$HEAL_LOG"; }
proc_start() {
    local e; e=\$(ps -o etimes= -p "\$1" 2>/dev/null | tr -d ' ')
    [ -n "\$e" ] || return 1
    echo \$(( \$(date +%s) - e ))
}
${FUNC_BODY}
restart_spira_collector_if_stale
DRIVER

    cat "$RESTART_LOG" 2>/dev/null || true
}

# ── Test 1: POSITIVE CONTROL — stale collector is restarted ───────────────────
# Process started 1000 s ago; cockpit.sh was updated 60 s ago (after process start).
# The watchdog must fire — silence here is indistinguishable from the old bug.
result=$(run_watchdog 1000 60)
want "stale collector: restart is requested" "spira-cockpit" "$result"
want "stale collector: instance-qualified unit (prod)" "prod" "$result"

# ── Test 2: NEGATIVE CONTROL — fresh collector is not restarted ───────────────
# Process started 60 s ago; cockpit.sh is 1000 s old (predates the process start).
result=$(run_watchdog 60 1000)
nowant "fresh collector: no restart" "spira-cockpit" "$result"

# ── Test 3: UNIT DISCOVERY — finds instance-qualified unit when plain is inactive
# MOCK_ACTIVE_SFX "cockpit-prod" matches spira-cockpit-prod.service but NOT
# spira-cockpit.service — so the plain unit is inactive, exactly as on a migrated box.
result=$(run_watchdog 1000 60 "prod" "cockpit-prod")
want "inactive plain unit: restart still fires" "cockpit-prod" "$result"

# ── Test 4: NO ACTIVE UNIT — watchdog returns without restarting ───────────────
result=$(run_watchdog 1000 60 "prod" "NONEXISTENT_UNIT_SUFFIX")
nowant "no active unit: no restart attempted" "cockpit" "$result"

# ── Test 5: REPLACED DURING A PASS (sp-vjiug, gap #10) ─────────────────────────
# The two tests above are each a single, static snapshot: source age and process age set
# once, then checked once. The untested case is the one a promotion actually produces: a
# long-running collector that was checked and found FRESH, and cockpit.sh is then replaced
# WHILE that same process keeps running with no restart in between — the watchdog's next
# check must catch the replacement using the process's ORIGINAL start time, not treat the
# process as new just because an earlier check passed it.
echo ""
echo "replaced during a pass: a promotion after an earlier fresh check is still caught"

PROC_START_LOG="$TMP/proc_start.log"
run_watchdog_twice() {
    # Same MOCK_PS_ELAPSED (so proc_start resolves to the same instant) across both calls —
    # one collector process, checked twice, exactly as the live timer does every minute.
    local elapsed_s="$1" src_age_1="$2" src_age_2="$3"
    rm -f "$RESTART_LOG" "$HEAL_LOG"
    local now; now=$(date +%s)
    touch -d "@$(( now - src_age_1 ))" "$PROD/cockpit.sh"
    PATH="$BIN:$PATH" SPIRA_INSTANCE=prod SPIRA_PROD="$PROD" \
        MOCK_ACTIVE_SFX=prod MOCK_RESTART_LOG="$RESTART_LOG" MOCK_PS_ELAPSED="$elapsed_s" \
        bash <<DRIVER
heal_log() { :; }
proc_start() {
    local e; e=\$(ps -o etimes= -p "\$1" 2>/dev/null | tr -d ' ')
    [ -n "\$e" ] || return 1
    echo \$(( \$(date +%s) - e ))
}
${FUNC_BODY}
restart_spira_collector_if_stale
DRIVER
    # cockpit.sh REPLACED — promoted a second time — while the collector process (per
    # MOCK_PS_ELAPSED, unchanged) has kept running the whole time with no restart between
    # the two checks.
    touch -d "@$(( now - src_age_2 ))" "$PROD/cockpit.sh"
    PATH="$BIN:$PATH" SPIRA_INSTANCE=prod SPIRA_PROD="$PROD" \
        MOCK_ACTIVE_SFX=prod MOCK_RESTART_LOG="$RESTART_LOG" MOCK_PS_ELAPSED="$elapsed_s" \
        bash <<DRIVER
heal_log() { :; }
proc_start() {
    local e; e=\$(ps -o etimes= -p "\$1" 2>/dev/null | tr -d ' ')
    [ -n "\$e" ] || return 1
    echo \$(( \$(date +%s) - e ))
}
${FUNC_BODY}
restart_spira_collector_if_stale
DRIVER
    cat "$RESTART_LOG" 2>/dev/null || true
}

# The process started 1000s ago. First promotion is 2000s old — predates the process, so the
# first check (not asserted on its own here) finds it fresh. The second promotion is 60s
# old — postdates the process — landing WHILE the same process is still running.
result=$(run_watchdog_twice 1000 2000 60)
want "second check after a mid-run promotion: restart fires" "spira-cockpit" "$result"

printf '\ntest-cockpit-collector-watchdog: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
