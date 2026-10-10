#!/usr/bin/env bash
#
# test-perf-watch-units.sh — perf-watch runs from a CPU-fenced oneshot on a repeating timer,
# its log is a watchd row, the manifest installs and enables both units, and the probe file
# names only commands that exist.
#
# tier: T0
# covers: systemd/spira-perf-watch.service systemd/spira-perf-watch.timer systemd/spira-perf-happy-path.* spira/perf-probes spira/watchers install/src/manifest.rs perf-watch/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"
SVC="$UNIT_DIR/spira-perf-watch.service"
TMR="$UNIT_DIR/spira-perf-watch.timer"

echo "=== service ==="
[ -r "$SVC" ] && ok "service readable" || bad "service readable" "not found at $SVC"
want "it is a oneshot" "Type=oneshot" "$(grep '^Type=' "$SVC" 2>/dev/null)"
want "ExecStart runs perf-watch on the probe file" "perf-watch --probes @SPIRA_HOME@/perf-probes" "$(grep '^ExecStart=' "$SVC" 2>/dev/null)"
_quota_re='^CPUQuota=[0-9]+%$'
is "positive control: the quota matcher finds a planted CPUQuota" "1" "$(printf 'CPUQuota=35%%\n' | grep -cE "$_quota_re")"
is "it is CPU-fenced" "1" "$(grep -cE "$_quota_re" "$SVC" 2>/dev/null)"
want "alarms land in the watched log" "@SPIRA_RUN@/perf-watch.log" "$(grep '^StandardOutput=' "$SVC" 2>/dev/null)"

echo "=== timer ==="
[ -r "$TMR" ] && ok "timer readable" || bad "timer readable" "not found at $TMR"
want "timer repeats" "OnUnitActiveSec=5min" "$(grep '^OnUnitActiveSec=' "$TMR" 2>/dev/null)"
want "timer arms itself at boot" "OnBootSec=" "$(grep '^OnBootSec=' "$TMR" 2>/dev/null)"
want "timer names the service" "spira-perf-watch.service" "$(grep '^Unit=' "$TMR" 2>/dev/null)"
if command -v systemd-analyze >/dev/null 2>&1; then
    want "the repeat interval parses as a timespan" "μs" "$(systemd-analyze timespan "$(grep '^OnUnitActiveSec=' "$TMR" | cut -d= -f2-)" 2>&1)"
fi

echo "=== manifest ==="
union_out="$(SPIRA_HOME="$UNIT_DIR/../spira" units-install --list-union 2>&1)"
units_block="$(printf '%s\n' "$union_out" | grep '^UNITS ')"
enable_block="$(printf '%s\n' "$union_out" | grep '^ENABLE ')"
want "positive control: units block is parseable" "spira-sentinel.timer" "$units_block"
want "service is in UNITS" "spira-perf-watch.service" "$units_block"
want "timer is in UNITS" "spira-perf-watch.timer" "$units_block"
want "timer is enabled" "spira-perf-watch.timer" "$enable_block"

echo "=== watcher row and probes ==="
want "the watchers file has a log row for the service's log" "perf-watch|log|@SPIRA_RUN@/perf-watch.log" "$(grep '^perf-watch|' "$HERE/watchers")"
probes="$(grep -v '^#' "$HERE/perf-probes" | grep .)"
is "the probe file names five hot paths" "5" "$(printf '%s\n' "$probes" | wc -l | tr -d ' ')"
bad_lines="$(printf '%s\n' "$probes" | grep -vP '^[a-z-]+\t\S+' )"
is "every probe is <name><TAB><command>" "" "$bad_lines"

echo "=== happy-path watcher ==="
HSVC="$UNIT_DIR/spira-perf-happy-path.service"
HTMR="$UNIT_DIR/spira-perf-happy-path.timer"
want "the happy-path watcher is a CPU-fenced oneshot" "Type=oneshot" "$(grep '^Type=' "$HSVC" 2>/dev/null)"
is "it is CPU-fenced" "1" "$(grep -cE "$_quota_re" "$HSVC" 2>/dev/null)"
want "it runs the happy subcommand" "perf-watch happy" "$(grep '^ExecStart=' "$HSVC" 2>/dev/null)"
want "it starts from the repo, which the sim world is built from" "WorkingDirectory=@SPIRA_REPO@" "$(grep '^WorkingDirectory=' "$HSVC" 2>/dev/null)"
want "its alarms land in the watched log" "@SPIRA_RUN@/perf-watch.log" "$(grep '^StandardOutput=' "$HSVC" 2>/dev/null)"
want "its timer repeats" "OnUnitActiveSec=" "$(grep '^OnUnitActiveSec=' "$HTMR" 2>/dev/null)"
want "its timer names the service" "spira-perf-happy-path.service" "$(grep '^Unit=' "$HTMR" 2>/dev/null)"
want "its service is in UNITS" "spira-perf-happy-path.service" "$units_block"
want "its timer is enabled" "spira-perf-happy-path.timer" "$enable_block"

tl_summary
