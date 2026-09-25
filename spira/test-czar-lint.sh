#!/usr/bin/env bash
#
# test-czar-lint.sh — static text checks for the czar shadow mechanism, split out of
# test-czar-shadow.sh (UC-safety-fences-31, gap 11): these 11 rows read a fayth, a brief and
# conf.sh's own allowlist rather than exercising any behaviour, so they cost nothing and need
# no fixture. The behavioural rows (shadow refuses, act allows, queue.sh eject wiring, class
# mapping) stay in test-czar-shadow.sh at T1.
#
# tier: T0
# covers: spira/conf.sh spira/chamber/czar.fayth spira/chamber/czar.md UC-safety-fences-31
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-czar-lint.sh"

# ==========================================================================================
echo
echo "czar.fayth FAYTH_TOOLS includes czar-fence.sh"
# ==========================================================================================
fayth_file="$HERE/chamber/czar.fayth"
if [ -f "$fayth_file" ]; then
    tools_line="$(grep '^FAYTH_TOOLS=' "$fayth_file" 2>/dev/null || true)"
    want "FAYTH_TOOLS includes czar-fence.sh" "czar-fence.sh" "$tools_line"
else
    bad "czar.fayth not found at $fayth_file" ""
fi

# ==========================================================================================
echo
echo "czar.md brief mentions CZAR-WOULD note format and czar-fence.sh"
# ==========================================================================================
brief_file="$HERE/chamber/czar.md"
if [ -f "$brief_file" ]; then
    content="$(cat "$brief_file" 2>/dev/null)"
    want "czar.md mentions CZAR-WOULD" "CZAR-WOULD" "$content"
    want "czar.md mentions shadow" "shadow" "$content"
    want "czar.md mentions czar-fence.sh" "czar-fence.sh" "$content"
else
    bad "czar.md not found at $brief_file" ""
fi

# ==========================================================================================
echo
echo "all 7 SPIRA_CZAR_STAGE_* keys in SPIRA_CONF_KEYS allowlist"
# ==========================================================================================
conf_sh="$HERE/conf.sh"
if [ -f "$conf_sh" ]; then
    for sfx in DEADLOCK ATTRIBUTION_FAILED SORT_FAILED LOOP_STALLED CI_STALLED STARVED CI_RED; do
        key="SPIRA_CZAR_STAGE_${sfx}"
        grep -q "$key" "$conf_sh" 2>/dev/null \
            && ok "$key in SPIRA_CONF_KEYS" \
            || bad "$key missing from SPIRA_CONF_KEYS" ""
    done
else
    bail "conf.sh not found at $conf_sh"
fi

tl_summary
