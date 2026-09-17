#!/usr/bin/env bash
#
# test-cockpit-layout-mail.sh — the cockpit's bottom-left pane is the mail client.
#
# `up` builds session, mail and a full-height health column; `ensure` brings a closed mail
# pane back; a client that is unset or not installed gets no pane rather than one that dies.
#
# covers: cockpit/layout.sh spira/conf.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

command -v tmux >/dev/null 2>&1 || { echo "  SKIP  tmux is not on PATH"; exit 77; }

TMP="$(mktemp -d)"
export TMUX_TMPDIR="$TMP/tmux"; mkdir -p "$TMUX_TMPDIR"; unset TMUX TMUX_PANE
cleanup() { tmux kill-server 2>/dev/null; rm -rf "$TMP"; }
trap cleanup EXIT INT TERM

# An installed-looking tree, because `ensure` refuses to run from anywhere but SPIRA_COCKPIT.
ROOT="$TMP/root"; mkdir -p "$ROOT/spira" "$ROOT/cockpit" "$TMP/bin"
cp "$HERE/conf.sh" "$ROOT/spira/"
cp "$HERE/../cockpit/layout.sh" "$HERE/../cockpit/tmux-env.sh" "$ROOT/cockpit/"
printf '#!/usr/bin/env bash\nsleep 300\n' > "$ROOT/cockpit/health.sh"
printf '#!/usr/bin/env bash\nsleep 300\n' > "$TMP/bin/fakemail"
chmod +x "$ROOT/cockpit/health.sh" "$TMP/bin/fakemail"

layout() {   # layout <COCKPIT_MAIL> <action> [args]
    local mail="$1"; shift
    env -i HOME="$TMP" PATH="$TMP/bin:/usr/bin:/bin" TMUX_TMPDIR="$TMUX_TMPDIR" \
        SPIRA_CONF="$TMP/no.conf" SPIRA_REPO="$TMP" SPIRA_COCKPIT="$ROOT/cockpit" \
        SPIRA_INSTANCE=fixture SPIRA_PROD="$TMP/noprod" SPIRA_LOOM_BIN="" \
        COCKPIT_CWD="$TMP" COCKPIT_BOTTOM_PCT=31 COCKPIT_MAIL="$mail" \
        bash "$ROOT/cockpit/layout.sh" "$@" 2>&1
}
panes() { tmux list-panes -t "$1" -F '#{@cockpit}|#{pane_id}|#{pane_left}|#{pane_top}|#{pane_height}|#{window_height}' 2>/dev/null; }
field() { panes "$1" | awk -F'|' -v t="$2" -v f="$3" '$1==t {print $f; exit}'; }

echo "test-cockpit-layout-mail.sh"

echo "up with a mail client: session, mail below it, full-height health"
tmux new-session -d -s c1 -x 200 -y 50
layout "$TMP/bin/fakemail" up --window c1:0 >/dev/null
sleep 0.5
is "three panes"                         3 "$(panes c1:0 | wc -l | tr -d ' ')"
is "one pane is tagged mail"             1 "$(panes c1:0 | awk -F'|' '$1=="mail"' | wc -l | tr -d ' ')"
is "mail sits in the left column"        0 "$(field c1:0 mail 3)"
[ "$(field c1:0 mail 4)" -gt 0 ] && ok "mail is below the session" || bad "mail is below the session" "top=$(field c1:0 mail 4)"
is "health keeps the full window height" "$(field c1:0 health 6)" "$(field c1:0 health 5)"

echo "ensure respawns a closed mail pane"
tmux kill-pane -t "$(field c1:0 mail 2)"
is "SEEN RED: the mail pane is gone"     "" "$(field c1:0 mail 2)"
layout "$TMP/bin/fakemail" ensure >/dev/null
sleep 0.5
is "ensure brought the mail pane back"   1 "$(panes c1:0 | awk -F'|' '$1=="mail"' | wc -l | tr -d ' ')"
is "and health is still full height"     "$(field c1:0 health 6)" "$(field c1:0 health 5)"

echo "no mail client configured: no mail pane"
tmux new-session -d -s c2 -x 200 -y 50
layout "" up --window c2:0 >/dev/null
sleep 0.3
is "two panes"                           2 "$(panes c2:0 | wc -l | tr -d ' ')"
is "none tagged mail"                    "" "$(field c2:0 mail 2)"

echo "a mail client that is not installed: no mail pane"
tmux new-session -d -s c3 -x 200 -y 50
out="$(layout no-such-mail-client up --window c3:0)"
sleep 0.3
is "two panes"                           2 "$(panes c3:0 | wc -l | tr -d ' ')"
is "none tagged mail"                    "" "$(field c3:0 mail 2)"

echo
printf 'test-cockpit-layout-mail: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
