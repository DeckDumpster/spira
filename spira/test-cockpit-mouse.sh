#!/usr/bin/env bash
# covers: cockpit/layout.sh spira/conf.sh
#
# The cockpit is panes the operator CLICKS INTO. tmux defaults `mouse` off and a box
# need not carry a ~/.tmux.conf, so without layout.sh setting it a click does nothing
# and reports nothing -- which reads as a dead attention panel rather than as an unset
# option. This suite pins that the cockpit sets it itself, and that an operator who
# prefers terminal-native drag-select can still turn it off.
#
# DEMOTED TO T1 (BEHAVIOUR, NOT SOURCE-GREP). apply_mouse_mode's opt-out case used to be
# asserted by grepping layout.sh for `off\|no\|0)` -- a pattern any case arm matches,
# whether or not it does what the arm says. Sourcing layout.sh and calling
# apply_mouse_mode with $TMUX_BIN pointed at a shim that records argv instead of a real
# server proves what the function actually DOES for each COCKPIT_MOUSE value, with no
# tmux server involved.
#
# The config-surface checks (key exists, defaults on, reaches layout.sh through the
# manifest) stay source greps: they are about wiring a key through conf.sh's allowlist,
# which has no runtime behaviour to call.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
LAYOUT="$HERE/../cockpit/layout.sh"
CONF="$HERE/conf.sh"

code() { grep -vE '^[[:space:]]*#' "$1"; }
CONF_CODE="$(code "$CONF")"
LAYOUT_CODE="$(code "$LAYOUT")"

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
echo "the call sites -- wiring, not behaviour, so this stays a grep:"
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
echo "behaviour: apply_mouse_mode, against a PATH shim instead of a real server"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

SHIM="$TMP/tmux-shim"
LOG="$TMP/argv.log"
# Records every invocation's argv, one line per call, then exits per $SHIM_RC (0 by
# default) so the case "it never fails the caller" can also make the shim fail.
cat > "$SHIM" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SHIM_LOG"
exit "${SHIM_RC:-0}"
SH
chmod +x "$SHIM"

# call_mouse <COCKPIT_MOUSE value, or "" for unset> [SHIM_RC] — sources layout.sh in a
# pinned environment and calls apply_mouse_mode once. Prints its exit code on stdout's
# last line and leaves $LOG holding whatever the shim recorded.
call_mouse() {
    local mouse="$1" rc="${2:-0}"
    : > "$LOG"
    local -a extra=()
    [ -n "$mouse" ] && extra=(COCKPIT_MOUSE="$mouse")
    env -i HOME="$TMP" PATH="/usr/bin:/bin" \
        SPIRA_REPO="$TMP" SPIRA_COCKPIT="$TMP/cockpit" SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=fixture SPIRA_LOOM_BIN="" COCKPIT_CWD="$TMP" \
        COCKPIT_BOTTOM_PCT=30 COCKPIT_RIGHT_PCT=33 COCKPIT_MAIL="" \
        TMUX_BIN="$SHIM" SHIM_LOG="$LOG" SHIM_RC="$rc" \
        "${extra[@]}" \
        bash -c '. "'"$LAYOUT"'"; apply_mouse_mode; echo "RC=$?"'
}

out="$(call_mouse "")"
want "default (unset): turns mouse on" "set-option -g mouse on" "$(cat "$LOG")"
want "default (unset): exits 0" "RC=0" "$out"

for off in off no 0; do
    out="$(call_mouse "$off")"
    [ ! -s "$LOG" ] && ok "COCKPIT_MOUSE=$off: tmux is never called" \
        || bad "COCKPIT_MOUSE=$off: tmux is never called" "shim saw: $(cat "$LOG")"
    want "COCKPIT_MOUSE=$off: exits 0" "RC=0" "$out"
done

out="$(call_mouse on)"
want "COCKPIT_MOUSE=on: turns mouse on" "set-option -g mouse on" "$(cat "$LOG")"

out="$(call_mouse on 1)"
want "the shim failing still exits 0: it never fails the caller" "RC=0" "$out"

tl_summary
