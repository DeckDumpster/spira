#!/usr/bin/env bash
#
# test-watch-unit-restart.sh — the watcher unit's restart limiter can actually fire.
#
#   ./test-watch-unit-restart.sh
#
# A watcher target that exits on every start — a second copy holding its lock — is respawned
# by Restart=always forever. The only thing that ends that is the burst limiter, and it can
# only fire if StartLimitBurst starts fit inside StartLimitIntervalSec. At RestartSec=15,
# five starts span at least 60s, so systemd's default 10s window can never hold them: the
# unit loops instead of landing in failed, and a loop is the one state that reads the same
# as healthy from `is-active`. Observed at 108 restarts over 18 hours.
#
# THE RELATIONSHIP IS WHAT IS ASSERTED, NOT THE NUMBERS. Raising RestartSec without raising
# the window silently restores the defect, and a test pinned to 120 would still pass.
#
# covers: systemd/spira-watch@.service
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
UNIT="$(dirname "$HERE")/systemd/spira-watch@.service"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }

echo "test-watch-unit-restart.sh"
echo

if [ ! -r "$UNIT" ]; then
    bad "the watcher unit template is readable" "not found at $UNIT"
    printf '\n  %d passed, %d failed\n' "$pass" "$fail"; exit 1
fi
ok "the watcher unit template is readable"

# Directives only. This file's comments state the very rule being checked, so a matcher over
# the whole file reads the explanation as readily as the setting (law-a-matcher-reads-code-
# not-prose).
code="$(grep -vE '^[[:space:]]*#' "$UNIT")"
val() { printf '%s\n' "$code" | sed -n "s/^[[:space:]]*$1=\([0-9][0-9]*\).*/\1/p" | tail -1; }

# THE POSITIVE CONTROL. Every comparison below is vacuous if these are unset: an empty
# string in an arithmetic test is not a failure a reader would notice.
restart="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*Restart=\([a-z-]*\).*/\1/p' | tail -1)"
rsec="$(val RestartSec)"
burst="$(val StartLimitBurst)"
window="$(val StartLimitIntervalSec)"
missing=""
[ -n "$restart" ] || missing="$missing Restart"
[ -n "$rsec" ]    || missing="$missing RestartSec"
[ -n "$burst" ]   || missing="$missing StartLimitBurst"
[ -n "$window" ]  || missing="$missing StartLimitIntervalSec"
if [ -n "$missing" ]; then
    bad "every restart directive is present (positive control)" "unset:$missing"
    printf '\n  %d passed, %d failed\n' "$pass" "$fail"; exit 1
fi
ok "every restart directive is present (positive control): Restart=$restart RestartSec=$rsec burst=$burst window=${window}s"

# THE LIMITER MUST BE REACHABLE. burst starts are spaced by (burst - 1) gaps of RestartSec.
need=$(( rsec * (burst - 1) ))
if [ "$window" -gt "$need" ]; then
    ok "the burst limiter can fire: ${burst} starts span ${need}s, window is ${window}s"
else
    bad "the burst limiter can fire" \
        "${burst} starts at RestartSec=${rsec} span ${need}s but the window is only ${window}s — the limiter never fires and a failing target loops forever"
fi

# THE LIMITER IS ONLY MEANINGFUL UNDER AN UNCONDITIONAL RESTART. Restart=on-failure would
# already stop on the clean exit this guards against, and the window would be decoration.
if [ "$restart" = always ]; then
    ok "the restart policy is unconditional, so the limiter is what stops a loop"
else
    bad "the restart policy is unconditional" "Restart=$restart — the limiter is not what stops a loop here"
fi

# IN [Unit], NOT [Service]. systemd ignores both directives under [Service] and says so only
# in the journal, which leaves the limiter silently at its defaults.
sect="$(printf '%s\n' "$code" | awk '/^\[/{s=$0} /^[[:space:]]*StartLimit/{print s}' | sort -u)"
if [ "$sect" = "[Unit]" ]; then
    ok "the StartLimit directives are in [Unit], where systemd reads them"
else
    bad "the StartLimit directives are in [Unit]" "found under: ${sect:-nothing}"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
