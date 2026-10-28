#!/usr/bin/env bash
# tier: T0
# covers: spira/*.sh *.sh cockpit/*.sh
#
# test-spira-config-cutover.sh — no retired config/rune parser remains in any non-test
# script. Test files are excluded: fixtures and per-surface suites name the patterns they check.
# cockpit.sh's direct RUNE_MODEL sed is a separate display mechanism and is not matched.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
HARNESS="$(cd "$HERE/.." && pwd -P)"
SELF="$(basename "$0")"
. "$HERE/testlib.sh"


files() {
    find "$HARNESS" -name '*.sh' -not -path '*/.git/*' -not -name "$SELF" \
        -not -name 'test-*.sh' 2>/dev/null
}


# POSITIVE CONTROLS FIRST: each pattern must match its own offender before its silence on
# the real tree is trusted (law-absence-needs-a-positive-control).
PC1='spira_conf_read() { :; }'
case "$PC1" in *'spira_conf_read('*) ok "PC: spira_conf_read pattern matches its own offender" ;;
               *) bad "PC: spira_conf_read pattern matches its own offender" "did not match" ;; esac

PC2='_lanes_col_idx() { :; }'
case "$PC2" in *'_lanes_col_idx'*) ok "PC: _lanes_col_idx pattern matches its own offender" ;;
               *) bad "PC: _lanes_col_idx pattern matches its own offender" "did not match" ;; esac

PC3='v="$(rune_get "$RUNE" RUNE_MODEL)"'
case "$PC3" in *'rune_get'*'RUNE_MODEL'*) ok "PC: rune_get RUNE_MODEL pattern matches its own offender" ;;
               *) bad "PC: rune_get RUNE_MODEL pattern matches its own offender" "did not match" ;; esac

hits="$(files | xargs grep -l 'spira_conf_read' 2>/dev/null)"
if [ -z "$hits" ]; then
    ok "no non-test file references spira_conf_read"
else
    bad "no non-test file references spira_conf_read" "found in: $(printf '%s' "$hits" | tr '\n' ' ')"
fi

hits="$(files | xargs grep -l '_lanes_col_idx' 2>/dev/null)"
if [ -z "$hits" ]; then
    ok "no non-test file references _lanes_col_idx (the retired column-position lanes engine)"
else
    bad "no non-test file references _lanes_col_idx" "found in: $(printf '%s' "$hits" | tr '\n' ' ')"
fi

hits="$(files | xargs grep -lE 'rune_get[^|]*RUNE_MODEL' 2>/dev/null)"
if [ -z "$hits" ]; then
    ok "no non-test file reads RUNE_MODEL through rune_get"
else
    bad "no non-test file reads RUNE_MODEL through rune_get" "found in: $(printf '%s' "$hits" | tr '\n' ' ')"
fi

hits="$(files | xargs grep -lF '${RUNE_MODEL:-' 2>/dev/null)"
if [ -z "$hits" ]; then
    ok "no non-test file falls back to a bare \${RUNE_MODEL:-...} literal"
else
    bad "no non-test file falls back to a bare \${RUNE_MODEL:-...} literal" "found in: $(printf '%s' "$hits" | tr '\n' ' ')"
fi

tl_summary
