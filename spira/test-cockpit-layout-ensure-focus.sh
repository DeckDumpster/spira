#!/usr/bin/env bash
#
# test-cockpit-layout-ensure-focus.sh — repair_dashboards restores the operator's actual
# focus, and only falls back to the session pane when that focus is gone.
#
# THE FAILURE THIS SUITE EXISTS FOR. repair_dashboards() was called from the `ensure`
# case (timer-driven, ~60s cadence) with an unconditional tmux select-pane at the end.
# This moved the operator's focus to the session pane every 60 seconds, even when no
# repairs were needed. The fix records the active pane at entry, restores it after
# repairs, and only selects the session pane if the original active pane was destroyed.
#
# THE ORIGINAL ASSERTION WAS VACUOUS. It compared #{pane_active} of the WINDOW'S
# active pane against itself — whichever pane tmux considers active always reports
# pane_active=1, so the case could not fail no matter what repair_dashboards did. This
# rewrite compares the active pane's #{pane_id} before and after, which is the only
# thing that can actually move, and adds the positive control: when the originally
# active pane really is gone, focus MUST move to the session pane.
#
# defect: sp-gyl8n
# tier: T2
# covers: cockpit/ops/src/layout.rs UC-cockpit-observability-41 UC-cockpit-observability-43 UC-cockpit-observability-44 UC-cockpit-observability-45
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
COCKPIT_DIR="$(dirname "$HERE")/cockpit"
# `layout` is a binary now (sp-llbmi); repair_dashboards is private to it, so this
# drives the real `ensure` subcommand (which calls repair_dashboards internally for
# every window cockpit_windows() finds) instead of sourcing the function directly.
# SPIRA_RELEASE points at a fake release whose bin/layout is the real compiled
# binary, so the installed-copy guard in `ensure` passes.
LAYOUT="layout"
_FAKE_RELEASE="$(mktemp -d)"
mkdir -p "$_FAKE_RELEASE/bin"
cp "$(command -v "$LAYOUT")" "$_FAKE_RELEASE/bin/layout"
cp "$(command -v health)" "$_FAKE_RELEASE/bin/health"
cp "$(command -v lc-view)" "$_FAKE_RELEASE/bin/lc-view"
# cockpit-ops's self_source() (cockpit/ops/src/conf.rs) resolves its registry at
# $SPIRA_RELEASE/spira/conf.d, never from a SPIRA_HOME env var — a fake release with no
# spira/ at all made every `layout ensure` below fall back to built-in defaults rather
# than this suite's own tl_config layer (one source of config, per Ryan 2026-10-05).
mkdir -p "$_FAKE_RELEASE/spira"
ln -s "$HERE/conf.d" "$_FAKE_RELEASE/spira/conf.d"

command -v tmux >/dev/null 2>&1 || { echo "  SKIP  tmux is not on PATH"; exit 77; }

TMP="$(mktemp -d)"
TMUXDIR="$TMP/tmux-fixture"
mkdir -p "$TMUXDIR"
FIXTURE_UP=0

# classify_argv's mail check compares argv[0]'s basename against COCKPIT_MAIL's — a
# shebang script exec'd directly would show up as "bash" there (the kernel interprets the
# shebang), same limitation the bash original had. `exec -a NAME` sets argv[0] to "fakemail"
# on a plain `sleep`, with no script/shebang involved at all. A file named "fakemail" still
# has to exist on PATH — Conf::from_env's `on_path` check never executes it, only checks
# `is_file()`.
printf '#!/usr/bin/env bash\ntrue\n' > "$_FAKE_RELEASE/bin/fakemail"
chmod +x "$_FAKE_RELEASE/bin/fakemail"

cleanup() {
    [ "$FIXTURE_UP" -eq 1 ] && TMUX_TMPDIR="$TMUXDIR" tmux kill-server 2>/dev/null || true
    rm -rf "$TMP" "$_FAKE_RELEASE"
}
trap cleanup EXIT

export TMUX_TMPDIR="$TMUXDIR"
unset TMUX TMUX_PANE

TMUX_TMPDIR="$TMUXDIR" tmux start-server
FIXTURE_UP=1

RUN="$TMP/.runtime"; mkdir -p "$RUN"

# well_formed <session-name> -> sets up a 3-pane window (session, health, mail — all
# tagged so repair_dashboards finds nothing to fix). Prints "<session-pane-id>
# <health-pane-id> <mail-pane-id>".
well_formed() {
    local sname="$1"
    TMUX_TMPDIR="$TMUXDIR" tmux new-session -d -s "$sname" -x 214 -y 53
    TMUX_TMPDIR="$TMUXDIR" tmux set-option -t "$sname" window-size largest

    local sess="$sname:0"
    local health mail
    health=$(TMUX_TMPDIR="$TMUXDIR" tmux split-window -t "$sess" -h -P -F '#{pane_id}' \
        "exec env PATH=$_FAKE_RELEASE/bin:\$PATH lc-view loop 10")
    mail=$(TMUX_TMPDIR="$TMUXDIR" tmux split-window -t "$sess:0" -v -P -F '#{pane_id}' \
        "exec -a fakemail sleep 300")
    local sess_id
    sess_id=$(TMUX_TMPDIR="$TMUXDIR" tmux list-panes -t "$sess" -F '#{pane_id}' | head -1)

    TMUX_TMPDIR="$TMUXDIR" tmux set-option -p -t "$health" @cockpit health
    TMUX_TMPDIR="$TMUXDIR" tmux set-option -p -t "$mail" @cockpit mail

    echo "$sess_id $health $mail"
}

# with_duplicate <session-name> -> a session pane plus TWO panes tagged health, as a
# respawn racing repair_dashboards can produce. repair_dashboards's own dedup step
# kills every health pane but the lowest-index one — this is the fixture that makes
# repair_dashboards itself destroy the pane the operator had focused, which is the only
# way the case this suite exists for actually happens. Prints "<session-pane-id>
# <kept-health-id> <duplicate-health-id-to-be-killed>".
with_duplicate() {
    local sname="$1"
    TMUX_TMPDIR="$TMUXDIR" tmux new-session -d -s "$sname" -x 214 -y 53
    TMUX_TMPDIR="$TMUXDIR" tmux set-option -t "$sname" window-size largest

    local sess="$sname:0"
    local h1 h2 sess_id
    h1=$(TMUX_TMPDIR="$TMUXDIR" tmux split-window -t "$sess" -h -P -F '#{pane_id}' \
        "exec env PATH=$_FAKE_RELEASE/bin:\$PATH lc-view loop 10")
    h2=$(TMUX_TMPDIR="$TMUXDIR" tmux split-window -t "$sess" -h -P -F '#{pane_id}' \
        "exec env PATH=$_FAKE_RELEASE/bin:\$PATH lc-view loop 10")
    sess_id=$(TMUX_TMPDIR="$TMUXDIR" tmux list-panes -t "$sess" -F '#{pane_id}' | head -1)

    TMUX_TMPDIR="$TMUXDIR" tmux set-option -p -t "$h1" @cockpit health
    TMUX_TMPDIR="$TMUXDIR" tmux set-option -p -t "$h2" @cockpit health

    echo "$sess_id $h1 $h2"
}

call_repair() {   # call_repair <window> -> nothing; runs the real `ensure`, which calls
                  # repair_dashboards internally for this window.
    local window="$1"
    # COCKPIT_MAIL/SPIRA_COCKPIT/SPIRA_RUN/SPIRA_INSTANCE/COCKPIT_CWD/COCKPIT_BOTTOM_PCT/
    # COCKPIT_RIGHT_PCT are registered keys (per Ryan 2026-10-05, ONE SOURCE OF CONFIG):
    # declare via tl_config and thread SPIRA_TOML through env -i, which clears it.
    tl_config COCKPIT_MAIL="fakemail" SPIRA_COCKPIT="$COCKPIT_DIR" SPIRA_RUN="$RUN" \
        SPIRA_INSTANCE=fixture COCKPIT_CWD="$TMP" COCKPIT_BOTTOM_PCT=30 COCKPIT_RIGHT_PCT=33
    # SPIRA_HOME is threaded through too, for parity with every other suite's env -i call —
    # cockpit-ops's self_source() does not actually read it (it derives its registry from
    # SPIRA_RELEASE/spira instead, fixed up above), but nothing reads SPIRA_HOME from env
    # any more regardless, so passing it costs nothing and keeps this call shaped like the
    # others.
    TMUX_TMPDIR="$TMUXDIR" env -i SPIRA_RELEASE="$_FAKE_RELEASE" HOME="$TMP" \
        PATH="$_FAKE_RELEASE/bin:/usr/bin:/bin" TMUX_TMPDIR="$TMUXDIR" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_TOML="$SPIRA_TOML" \
        "$LAYOUT" ensure 2>/dev/null || true
}

active_of() {   # active_of <window> -> pane_id of the active pane
    TMUX_TMPDIR="$TMUXDIR" tmux display-message -t "$1" -p '#{pane_id}' 2>/dev/null
}

echo "test-cockpit-layout-ensure-focus.sh"

echo
echo "no repair needed: focus on a non-session pane survives repair_dashboards"
read -r sess_id health mail < <(well_formed w1)
TMUX_TMPDIR="$TMUXDIR" tmux select-pane -t "$health"
before="$(active_of w1:0)"
is "fixture: health pane is active before the call" "$health" "$before"
call_repair "w1:0"
after="$(active_of w1:0)"
is "active pane (by id) is unchanged when nothing needed repair" "$before" "$after"

echo
echo "positive control: repair_dashboards' own dedup kills the pane the operator had"
echo "focused -- focus must fall back to the session pane, not stay on the corpse"
read -r sess_id2 kept dup < <(with_duplicate w2)
TMUX_TMPDIR="$TMUXDIR" tmux select-pane -t "$dup"
before2="$(active_of w2:0)"
is "fixture: the soon-to-be-killed duplicate is active before the call" "$dup" "$before2"
call_repair "w2:0"
still_there="$(TMUX_TMPDIR="$TMUXDIR" tmux list-panes -t w2:0 -F '#{pane_id}' | grep -Fxc "$dup")" || still_there=0
is "the duplicate really was killed by repair_dashboards" "0" "$still_there"
after2="$(active_of w2:0)"
is "focus falls back to the session pane once its target is gone" "$sess_id2" "$after2"

echo
echo "a copy of the layout binary refuses ensure and names the installed one"
ensure_stderr() {   # ensure_stderr <layout-binary> [KEY=value ...] -> that run's output
    local bin="$1"; shift
    tl_config COCKPIT_MAIL="fakemail" SPIRA_COCKPIT="$COCKPIT_DIR" SPIRA_RUN="$RUN" \
        SPIRA_INSTANCE=fixture COCKPIT_CWD="$TMP" COCKPIT_BOTTOM_PCT=30 COCKPIT_RIGHT_PCT=33 "$@"
    TMUX_TMPDIR="$TMUXDIR" env -i SPIRA_RELEASE="$_FAKE_RELEASE" HOME="$TMP" \
        PATH="$_FAKE_RELEASE/bin:/usr/bin:/bin" TMUX_TMPDIR="$TMUXDIR" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" SPIRA_TOML="$SPIRA_TOML" \
        "$bin" ensure 2>&1
}
mkdir -p "$TMP/copy"
cp "$_FAKE_RELEASE/bin/layout" "$TMP/copy/layout"
installed_err="$(ensure_stderr "$_FAKE_RELEASE/bin/layout")"
nowant "positive control: the installed binary is not refused" "ensure refused" "$installed_err"
copy_err="$(ensure_stderr "$TMP/copy/layout")"
want   "a copy refuses ensure"                        "ensure refused" "$copy_err"
want   "and names the installed binary's path"        "/bin/layout ensure" "$copy_err"

echo
echo "mouse mode: left alone by off/no/0, and ensure still succeeds"
mouse_now() { TMUX_TMPDIR="$TMUXDIR" tmux show-options -gv mouse 2>/dev/null; }
for v in off no 0; do
    TMUX_TMPDIR="$TMUXDIR" tmux set-option -g mouse off
    ensure_stderr "$_FAKE_RELEASE/bin/layout" COCKPIT_MOUSE="$v" >/dev/null
    is "COCKPIT_MOUSE=$v leaves mouse off" "off" "$(mouse_now)"
done

echo
echo "pane identity: a tag on a pane that runs no dashboard is cleared, not obeyed"
TMUX_TMPDIR="$TMUXDIR" tmux new-session -d -s w3 -x 214 -y 53
TMUX_TMPDIR="$TMUXDIR" tmux set-option -t w3 window-size largest
sess3="$(TMUX_TMPDIR="$TMUXDIR" tmux list-panes -t w3:0 -F '#{pane_id}' | head -1)"
imp="$(TMUX_TMPDIR="$TMUXDIR" tmux split-window -t w3:0 -h -P -F '#{pane_id}' "sh -c 'sleep 300; :' health.sh")"
TMUX_TMPDIR="$TMUXDIR" tmux set-option -p -t "$imp" @cockpit health
is "positive control: the impostor carries the health tag before ensure" "health" \
    "$(TMUX_TMPDIR="$TMUXDIR" tmux display-message -p -t "$imp" '#{@cockpit}')"
imp_out="$(ensure_stderr "$_FAKE_RELEASE/bin/layout")"
is   "ensure untagged the impostor" "" \
    "$(TMUX_TMPDIR="$TMUXDIR" tmux display-message -p -t "$imp" '#{@cockpit}')"
want "and logged it" "runs no dashboard" "$imp_out"
panes3="$(TMUX_TMPDIR="$TMUXDIR" tmux list-panes -t w3:0 -F '#{pane_id}')"
want "the session pane was not killed"  "$sess3" "$panes3"
want "the impostor pane was not killed" "$imp"   "$panes3"

tl_summary
