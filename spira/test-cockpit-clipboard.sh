#!/usr/bin/env bash
# tier: T1
# covers: cockpit/layout.sh spira/conf.sh
#
# sp-gg587: copying a mouse selection out of the nested concierge client reaches the
# operator's terminal only if the OUTER (layout/cockpit) server relays OSC 52, and tmux
# defaults `set-clipboard` off. This suite pins that layout.sh turns it on itself, on both
# `up` and `ensure` (so a rebuilt server — which starts from tmux's default — gets it back),
# and that COCKPIT_CLIPBOARD=off leaves the server default alone.
#
# BEHAVIOUR, NOT SOURCE-GREP, for apply_clipboard_mode itself: sourcing layout.sh and calling
# it with $TMUX_BIN pointed at a shim that records argv proves what the function actually
# asks tmux to do, mirroring test-cockpit-mouse.sh's apply_mouse_mode coverage.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
LAYOUT="$HERE/../cockpit/layout.sh"
CONF="$HERE/conf.sh"

code() { grep -vE '^[[:space:]]*#' "$1"; }
CONF_CODE="$(code "$CONF")"
LAYOUT_CODE="$(code "$LAYOUT")"

echo "test-cockpit-clipboard.sh"
echo

echo "the key exists and is on by default:"
if grep -qE '\$\{COCKPIT_CLIPBOARD:=on\}' <<< "$CONF_CODE"; then
    ok "COCKPIT_CLIPBOARD defaults to on"
else
    bad "COCKPIT_CLIPBOARD defaults to on" "no :=on default in conf.sh; a rebuilt server would come up unable to relay a copy"
fi
# A key absent from the allowlist is silently ignored when set in spira.conf --
# the failure the allowlist exists to prevent happening to a typo.
if grep -q 'COCKPIT_CLIPBOARD' <<< "$CONF_CODE"; then
    _n="$(grep -c 'COCKPIT_CLIPBOARD' <<< "$CONF_CODE")"
    if [ "${_n:-0}" -ge 3 ]; then
        ok "COCKPIT_CLIPBOARD is in the manifest, the defaults and the export list"
    else
        bad "COCKPIT_CLIPBOARD is in the manifest, the defaults and the export list" \
            "found $_n of the 3 places; setting it in spira.conf would be ignored, or it would not reach layout.sh"
    fi
else
    bad "COCKPIT_CLIPBOARD is in the manifest, the defaults and the export list" "absent from conf.sh"
fi

echo
echo "the call sites -- wiring, not behaviour, so this stays a grep:"
for verb in up ensure; do
    _block="$(awk -v v="$verb" '$0 ~ "^"v"\\)" {f=1} f {print} f && /^    ;;/ {exit}' \
        <<< "$LAYOUT_CODE")"
    if grep -q 'apply_clipboard_mode' <<< "$_block"; then
        ok "the $verb path applies it"
    else
        bad "the $verb path applies it" "apply_clipboard_mode is not called from $verb"
    fi
done

echo
echo "behaviour: apply_clipboard_mode, against a PATH shim instead of a real server"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

SHIM="$TMP/tmux-shim"
LOG="$TMP/argv.log"
cat > "$SHIM" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SHIM_LOG"
exit "${SHIM_RC:-0}"
SH
chmod +x "$SHIM"

# call_clipboard <COCKPIT_CLIPBOARD value, or "" for unset> [SHIM_RC] — sources layout.sh in
# a pinned environment and calls apply_clipboard_mode once.
call_clipboard() {
    local val="$1" rc="${2:-0}"
    : > "$LOG"
    local -a extra=()
    [ -n "$val" ] && extra=(COCKPIT_CLIPBOARD="$val")
    env -i HOME="$TMP" PATH="/usr/bin:/bin" \
        SPIRA_REPO="$TMP" SPIRA_COCKPIT="$TMP/cockpit" SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=fixture SPIRA_LOOM_BIN="" COCKPIT_CWD="$TMP" \
        COCKPIT_BOTTOM_PCT=30 COCKPIT_RIGHT_PCT=33 COCKPIT_MAIL="" \
        TMUX_BIN="$SHIM" SHIM_LOG="$LOG" SHIM_RC="$rc" \
        "${extra[@]}" \
        bash -c '. "'"$LAYOUT"'"; apply_clipboard_mode; echo "RC=$?"'
}

out="$(call_clipboard "")"
want "default (unset): turns set-clipboard on" "set-option -g set-clipboard on" "$(cat "$LOG")"
want "default (unset): exits 0" "RC=0" "$out"

for off in off no 0; do
    out="$(call_clipboard "$off")"
    [ ! -s "$LOG" ] && ok "COCKPIT_CLIPBOARD=$off: tmux is never called" \
        || bad "COCKPIT_CLIPBOARD=$off: tmux is never called" "shim saw: $(cat "$LOG")"
    want "COCKPIT_CLIPBOARD=$off: exits 0" "RC=0" "$out"
done

out="$(call_clipboard on)"
want "COCKPIT_CLIPBOARD=on: turns set-clipboard on" "set-option -g set-clipboard on" "$(cat "$LOG")"

out="$(call_clipboard on 1)"
want "the shim failing still exits 0: it never fails the caller" "RC=0" "$out"

tl_summary
