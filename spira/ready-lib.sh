# ready-lib.sh — Sourced, never executed. ready.sh's outcome rows, exit rule and timers check,
# reading nothing but their arguments and the program "$SC" names.

pass=0; warn=0; fail=0; unkn=0; skip=0
PASS() { printf '  pass  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; pass=$((pass+1)); }
WARN() { printf '  WARN  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; warn=$((warn+1)); }
FAIL() { printf '  FAIL  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; fail=$((fail+1)); }
UNKN() { printf '  ?     %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; unkn=$((unkn+1)); }
# SKIP: component deliberately not installed; counts as neither pass nor fail.
SKIP() { printf '  skip  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; skip=$((skip+1)); }

# ready_timers <unit dir> <instance> — one row per active spira-*-<instance>.timer in the dir.
#
# An elapsed timer that never rearmed is still "active", so is-active cannot see it; only then
# are both NextElapseUSec properties unset (empty or 0).
ready_timers() {
    local dir="$1" inst="$2" checked=0 tf tu next_r next_m
    if [ -d "$dir" ]; then
        for tf in "$dir"/spira-*-"$inst".timer; do
            [ -e "$tf" ] || continue
            tu="${tf##*/}"
            "$SC" --user is-active --quiet "$tu" 2>/dev/null || continue
            checked=$((checked+1))
            next_r="$("$SC" --user show -p NextElapseUSecRealtime --value "$tu" 2>/dev/null)"
            next_m="$("$SC" --user show -p NextElapseUSecMonotonic --value "$tu" 2>/dev/null)"
            if { [ -z "$next_r" ] || [ "$next_r" = "0" ]; } && \
               { [ -z "$next_m" ] || [ "$next_m" = "0" ]; }; then
                FAIL "$tu is active but has stopped scheduling (elapsed, no next trigger)" \
                     "systemctl --user status $tu
    Likely cause: the user manager restarted after OnBootSec's deadline had already
    passed. Restart it: systemctl --user restart $tu"
            else
                PASS "$tu still scheduled"
            fi
        done
    fi
    [ "$checked" -eq 0 ] && SKIP "timers — no installed spira-*-$inst.timer found"
    return 0
}

# ready_verdict — the summary line; returns 1 when any row was FAIL or ?, else 0. A WARN or a
# skip alone is still ready.
ready_verdict() {
    if [ "$fail" -gt 0 ] || [ "$unkn" -gt 0 ]; then
        printf '%d pass, %d WARN, %d FAIL, %d unknown, %d skip — NOT READY: loop may not receive work.\n' \
            "$pass" "$warn" "$fail" "$unkn" "$skip"
        return 1
    fi
    printf '%d pass, %d WARN, %d skip — armed: loop is ready to receive work.\n' "$pass" "$warn" "$skip"
    return 0
}
