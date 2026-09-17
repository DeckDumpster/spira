#!/usr/bin/env bash
# test-unit-restart-limits.sh — every restarting service unit has a reachable start limiter.
#
# A Restart=always unit with RestartSec=N can never trip systemd's default 10s limiter
# when N*(burst-1) > 10, so the burst never accumulates and the unit loops instead of
# landing in failed. StartLimitIntervalSec must exceed RestartSec*(StartLimitBurst-1), and
# the directives must live in [Unit] — systemd silently ignores them under [Service].
#
# covers: systemd/*.service
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd -P)"
SYSTEMD="$(dirname "$HERE")/systemd"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }

echo "test-unit-restart-limits.sh"
echo

# check_unit FILE
# Examines directives only (strips comments). Returns 0 and prints a summary if the unit's
# limiter is reachable; returns 1 and prints the reason if not. Returns 0 with no output
# for units that do not restart.
check_unit() {
    local unit="$1"
    local code restart rsec burst window

    code="$(grep -vE '^[[:space:]]*#' "$unit")"
    restart="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*Restart=\([a-z-]*\).*/\1/p' | tail -1)"

    case "$restart" in
        always|on-failure) ;;
        *) return 0 ;;
    esac

    rsec="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*RestartSec=\([0-9][0-9]*\).*/\1/p' | tail -1)"
    burst="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*StartLimitBurst=\([0-9][0-9]*\).*/\1/p' | tail -1)"
    window="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*StartLimitIntervalSec=\([0-9][0-9]*\).*/\1/p' | tail -1)"

    local missing=""
    [ -n "$rsec" ]    || missing="$missing RestartSec"
    [ -n "$burst" ]   || missing="$missing StartLimitBurst"
    [ -n "$window" ]  || missing="$missing StartLimitIntervalSec"
    if [ -n "$missing" ]; then
        printf 'missing:%s\n' "$missing"
        return 1
    fi

    local need
    need=$(( rsec * (burst - 1) ))
    if [ "$window" -le "$need" ]; then
        printf '%d starts at RestartSec=%ds span %ds but StartLimitIntervalSec=%ds — limiter never fires\n' \
            "$burst" "$rsec" "$need" "$window"
        return 1
    fi

    local sect
    sect="$(printf '%s\n' "$code" | awk '/^\[/{s=$0} /^[[:space:]]*StartLimit/{print s}' | sort -u)"
    if [ "$sect" != "[Unit]" ]; then
        printf 'StartLimit directives under %s, not [Unit] — systemd ignores them there\n' "${sect:-nothing}"
        return 1
    fi

    printf 'Restart=%s RestartSec=%ds burst=%d window=%ds\n' "$restart" "$rsec" "$burst" "$window"
    return 0
}

# NEGATIVE CONTROL. The checker must flag a unit whose window cannot hold burst starts.
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

cat > "$tmpdir/unreachable-limiter.service" <<'EOF'
[Unit]
Description=Test unit with unreachable limiter
StartLimitIntervalSec=10
StartLimitBurst=5

[Service]
Type=simple
ExecStart=/bin/true
Restart=always
RestartSec=15
EOF

if out="$(check_unit "$tmpdir/unreachable-limiter.service" 2>&1)"; then
    bad "negative control: checker rejects unreachable limiter" "checker passed when it should fail: ${out:-<no output>}"
else
    ok "negative control: checker rejects unreachable limiter (window=10s < 5 starts at RestartSec=15s)"
fi

# ALL SHIPPED TEMPLATES.
found=0
for unit in "$SYSTEMD"/*.service; do
    name="$(basename "$unit")"
    code="$(grep -vE '^[[:space:]]*#' "$unit")"
    restart="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*Restart=\([a-z-]*\).*/\1/p' | tail -1)"
    case "$restart" in
        always|on-failure) ;;
        *) continue ;;
    esac
    found=$(( found + 1 ))
    if result="$(check_unit "$unit" 2>&1)"; then
        ok "$name: $result"
    else
        bad "$name" "$result"
    fi
done

if [ "$found" -eq 0 ]; then
    bad "at least one restarting unit exists in systemd/" "none found — positive control missing"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
