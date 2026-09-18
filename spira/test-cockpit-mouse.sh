#!/usr/bin/env bash
# covers: cockpit/layout.sh spira/conf.sh
#
# The cockpit is panes the operator CLICKS INTO. tmux defaults `mouse` off and a box
# need not carry a ~/.tmux.conf, so without layout.sh setting it a click does nothing
# and reports nothing -- which reads as a dead attention panel rather than as an unset
# option. This suite pins that the cockpit sets it itself, and that an operator who
# prefers terminal-native drag-select can still turn it off.
#
# Asserted against the SCRIPT and the CONFIG, not against a live tmux server: a suite
# that drove the real server would flip a setting under whoever is attached to it.
#
# MATCHERS READ CODE, NOT PROSE (law-a-matcher-reads-code-not-prose). The comments in
# layout.sh and conf.sh name `mouse` and COCKPIT_MOUSE repeatedly to explain why they
# are there, so a whole-file grep matches the explanation as readily as the mechanism
# and passes against a file it was deleted from.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
LAYOUT="$HERE/../cockpit/layout.sh"
CONF="$HERE/conf.sh"

code() { grep -vE '^[[:space:]]*#' "$1"; }
LAYOUT_CODE="$(code "$LAYOUT")"
CONF_CODE="$(code "$CONF")"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }

echo "test-cockpit-mouse.sh"
echo

echo "the key exists and is on by default:"
if grep -qE '\$\{COCKPIT_MOUSE:=on\}' <<< "$CONF_CODE"; then
    ok "COCKPIT_MOUSE defaults to on"
else
    bad "COCKPIT_MOUSE defaults to on" "no :=on default in conf.sh; a fresh box gets an unclickable cockpit"
fi
# A key absent from the allowlist is silently ignored when set in spira.conf --
# the failure the allowlist exists to prevent happening to a typo.
if grep -q 'COCKPIT_MOUSE' <<< "$CONF_CODE"; then
    _n="$(grep -c 'COCKPIT_MOUSE' <<< "$CONF_CODE")"
    if [ "${_n:-0}" -ge 3 ]; then
        ok "COCKPIT_MOUSE is in the manifest, the defaults and the export list"
    else
        bad "COCKPIT_MOUSE is in the manifest, the defaults and the export list" \
            "found $_n of the 3 places; setting it in spira.conf would be ignored, or it would not reach layout.sh"
    fi
else
    bad "COCKPIT_MOUSE is in the manifest, the defaults and the export list" "absent from conf.sh"
fi

echo
echo "layout.sh turns it on itself, rather than trusting the operator's dotfiles:"
if grep -qE 'set-option +-g +mouse +on' <<< "$LAYOUT_CODE"; then
    ok "layout.sh sets mouse on"
else
    bad "layout.sh sets mouse on" "no 'set-option -g mouse on'; the cockpit depends on a ~/.tmux.conf that need not exist"
fi

# ENSURE IS THE ONE THAT MATTERS. A tmux server restarted by hand comes back with
# mouse off, and `up` may never run on it again -- so a cockpit that has only been
# ensured would stay unclickable forever.
for verb in up ensure; do
    _block="$(awk -v v="$verb" '$0 ~ "^"v"\\)" {f=1} f {print} f && /^    ;;/ {exit}' \
        <<< "$LAYOUT_CODE")"
    if grep -q 'apply_mouse_mode' <<< "$_block"; then
        ok "the $verb path applies it"
    else
        bad "the $verb path applies it" "apply_mouse_mode is not called from $verb"
    fi
done

echo
echo "the operator can still refuse it:"
if grep -qE 'off\|no\|0\)' <<< "$LAYOUT_CODE"; then
    ok "COCKPIT_MOUSE=off opts out"
else
    bad "COCKPIT_MOUSE=off opts out" "no opt-out branch; mouse mode costs terminal-native drag-select and that is per-operator"
fi

# It is called from a timer. A cosmetic option must never fail the caller.
if grep -qE 'set-option +-g +mouse +on[^|]*\|\| *true' <<< "$LAYOUT_CODE"; then
    ok "it never fails the caller"
else
    bad "it never fails the caller" "an unclickable cockpit is degraded, not broken; layout.sh runs from a timer"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
