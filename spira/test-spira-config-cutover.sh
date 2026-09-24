#!/usr/bin/env bash
#
# test-spira-config-cutover.sh — tree-wide T0 sweep (sp-zs04v.5): none of the parsers this
# cutover retires remain anywhere in the tree, not just in the one file each per-surface
# suite already checks.
#
# WHY THIS IS ITS OWN SUITE, NOT A DUPLICATE
# -------------------------------------------
# test-conf-toml.sh checks conf.sh; test-repo-map-toml.sh checks lib.sh; test-persona-model.sh
# checks aeon.sh and concierge.sh. Each proves its own surface clean, but a straggler caller
# in a fourth file — doctor.sh, cockpit.sh, a systemd unit script — would pass all three and
# still be a live parser. This suite greps every *.sh file in the tree instead of one each.
#
# EXCLUDES test-*.sh FILES. Fixtures and the per-surface suites themselves legitimately
# contain these exact strings — a fayth fixture with a literal `FAYTH_MODEL=...` line, or a
# grep pattern that names what it is checking for. Checking test files would flag the checks.
#
# DOES NOT FORBID FAYTH_MODEL EVERYWHERE. cockpit.sh reads a fayth's FAYTH_MODEL directly (a
# sed, not fayth_get) to display the persona's declared SEED value beside its resolved launch
# model — a deliberately separate, still-live mechanism (sp-zs04v.6), not the file-scraping
# this bead retires. Only the retired PATTERNS are checked: spira_conf_read, the repo-map's
# column-position lanes engine, and fayth_get read as a MODEL launch resolver.
#
# defect: sp-zs04v.5
# covers: spira/*.sh *.sh cockpit/*.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
HARNESS="$(cd "$HERE/.." && pwd -P)"
SELF="$(basename "$0")"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-spira-config-cutover.sh"

files() {
    find "$HARNESS" -name '*.sh' -not -path '*/.git/*' -not -name "$SELF" \
        -not -name 'test-*.sh' 2>/dev/null
}

# ==========================================================================
echo
echo "T0 — no retired parser remains outside the per-surface suites' own files:"
# ==========================================================================

# POSITIVE CONTROLS FIRST: each pattern must match its own offender before its silence on
# the real tree is trusted (law-absence-needs-a-positive-control).
PC1='spira_conf_read() { :; }'
case "$PC1" in *'spira_conf_read('*) ok "PC: spira_conf_read pattern matches its own offender" ;;
               *) bad "PC: spira_conf_read pattern matches its own offender" "did not match" ;; esac

PC2='_lanes_col_idx() { :; }'
case "$PC2" in *'_lanes_col_idx'*) ok "PC: _lanes_col_idx pattern matches its own offender" ;;
               *) bad "PC: _lanes_col_idx pattern matches its own offender" "did not match" ;; esac

PC3='v="$(fayth_get "$FAYTH" FAYTH_MODEL)"'
case "$PC3" in *'fayth_get'*'FAYTH_MODEL'*) ok "PC: fayth_get FAYTH_MODEL pattern matches its own offender" ;;
               *) bad "PC: fayth_get FAYTH_MODEL pattern matches its own offender" "did not match" ;; esac

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

hits="$(files | xargs grep -lE 'fayth_get[^|]*FAYTH_MODEL' 2>/dev/null)"
if [ -z "$hits" ]; then
    ok "no non-test file reads FAYTH_MODEL through fayth_get"
else
    bad "no non-test file reads FAYTH_MODEL through fayth_get" "found in: $(printf '%s' "$hits" | tr '\n' ' ')"
fi

hits="$(files | xargs grep -lF '${FAYTH_MODEL:-' 2>/dev/null)"
if [ -z "$hits" ]; then
    ok "no non-test file falls back to a bare \${FAYTH_MODEL:-...} literal"
else
    bad "no non-test file falls back to a bare \${FAYTH_MODEL:-...} literal" "found in: $(printf '%s' "$hits" | tr '\n' ' ')"
fi

# ==========================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
