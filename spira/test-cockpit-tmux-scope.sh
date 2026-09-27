#!/usr/bin/env bash
#
# test-cockpit-tmux-scope.sh — cockpit tools reach the cockpit's own server, never
# whichever one the invoking shell happens to be attached to.
#
# THE FAILURE THIS SUITE EXISTS FOR (sp-tc9ha correction, sp-1biss). layout.sh and
# rebuild.sh shelled out to bare `tmux`, which prefers $TMUX — the server the CALLER's
# shell is attached to — over the default socket. Run by hand from inside an unrelated
# tmux session (a human's, or an agent's own), the tool silently inspected and rebuilt
# THAT session's server instead of the cockpit's, while the real cockpit sat unrepaired.
# The workaround was `env -u TMUX layout.sh up`; the fix bakes that into the tools.
#
# FIXTURE: two real tmux servers under one TMUX_TMPDIR — the REAL one on the default
# socket (no -L), standing in for the cockpit's own server, and a DECOY on a named
# socket. Both carry a same-named window/session so a wrong-server hit is invisible
# except by inspecting which server actually changed. $TMUX is set to the decoy's
# socket, exactly as it would be inside a shell attached to it, before invoking each
# tool — simulating a human running the tool from inside that other session.
#
# defect: sp-1biss
# covers: cockpit/layout.sh cockpit/rebuild.sh
# hermetic-ok: two servers of its own under one TMUX_TMPDIR; touches no operator state
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
COCKPIT_DIR="$HERE/../cockpit"
LAYOUT="$COCKPIT_DIR/layout.sh"
REBUILD="$COCKPIT_DIR/rebuild.sh"

command -v tmux >/dev/null 2>&1 || skip "tmux not available"

T="$(mktemp -d)"
cleanup() {
    TMUX_TMPDIR="$T" tmux kill-server 2>/dev/null || true
    TMUX_TMPDIR="$T" tmux -L decoy kill-server 2>/dev/null || true
    rm -rf "$T"
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM

export TMUX_TMPDIR="$T"
unset TMUX TMUX_PANE

echo "test-cockpit-tmux-scope.sh"

# ======================================================================================
echo
echo "1. layout.sh up: a decoy \$TMUX must not steer the repair onto the decoy server"
# ======================================================================================
RUN1="$T/run"; mkdir -p "$RUN1"
FAKE_COCK="$T/fake-cockpit"; mkdir -p "$FAKE_COCK"
printf '#!/usr/bin/env bash\nsleep 600\n' > "$FAKE_COCK/health.sh"
chmod +x "$FAKE_COCK/health.sh"

# REAL: the default socket (no -L) — stands in for the cockpit's own server. Named
# "brain", not "cockpit", so section 2 below is free to use "cockpit" as the session
# whose presence tells real and decoy apart.
tmux new-session -d -s brain -x 200 -y 50 -c "$T"
real_before="$(tmux list-panes -t brain:0 -F '#{pane_id}' | wc -l)"
is "fixture: real session starts with one pane" "1" "$real_before"

# DECOY: a named socket, same session/window name, so a wrong-server hit would
# otherwise look identical from outside.
tmux -L decoy new-session -d -s brain -x 200 -y 50 -c "$T"
decoy_before="$(tmux -L decoy list-panes -t brain:0 -F '#{pane_id}' | wc -l)"
is "fixture: decoy session starts with one pane" "1" "$decoy_before"

# The exact string a shell attached to the decoy would carry in $TMUX.
decoy_sock="$(tmux -L decoy display-message -p '#{socket_path}')"
decoy_pid="$(tmux -L decoy display-message -p '#{pid}')"
decoy_pane="$(tmux -L decoy list-panes -t brain:0 -F '#{pane_id}' | head -1)"
DECOY_TMUX="${decoy_sock},${decoy_pid},0"
if [ -z "$decoy_sock" ] || [ -z "$decoy_pid" ]; then
    bail "fixture: could not read the decoy server's own socket/pid"
fi

TMUX="$DECOY_TMUX" TMUX_PANE="$decoy_pane" \
SPIRA_COCKPIT="$FAKE_COCK" SPIRA_REPO="$T" SPIRA_RUN="$RUN1" SPIRA_HOME="$HERE" \
SPIRA_CONF="$T/no.conf" COCKPIT_MAIL="" \
    bash "$LAYOUT" up --window brain:0 >/dev/null 2>&1
rc=$?
is "up exits 0 even with a decoy \$TMUX in the environment" "0" "$rc"

real_tags="$(tmux list-panes -t brain:0 -F '#{@cockpit}' 2>/dev/null | sort | tr '\n' ' ')"
want "the REAL server got the repair: a pane is tagged health" "health" "$real_tags"

decoy_after="$(tmux -L decoy list-panes -t brain:0 -F '#{pane_id}' | wc -l)"
is "the DECOY server is untouched: still one pane" "1" "$decoy_after"
decoy_tags="$(tmux -L decoy list-panes -t brain:0 -F '#{@cockpit}' 2>/dev/null | tr -d '[:space:]')"
is "the DECOY server carries no @cockpit tag" "" "$decoy_tags"

# ======================================================================================
echo
echo "2. rebuild.sh probe: a decoy \$TMUX must not steer the report onto the decoy server"
# ======================================================================================
# Non-default COCKPIT_SESSIONS so a pass cannot be explained by a hardcoded list.
SESSLIST="brain hunk chat ops"
for s in $SESSLIST; do tmux new-session -d -s "$s" -c "$T"; done
# The REAL server never gets a "cockpit" session — probe against it must say MISSING.

for s in $SESSLIST cockpit; do tmux -L decoy new-session -d -s "$s" -c "$T" 2>/dev/null || true; done
# The DECOY carries every session, including "cockpit" — if probe read the decoy
# instead of the real server, it would wrongly report "cockpit" present.

out="$(TMUX="$DECOY_TMUX" TMUX_PANE="$decoy_pane" \
    SPIRA_CONF="$T/no.conf" COCKPIT_SESSIONS="$SESSLIST" \
    bash "$REBUILD" probe 2>&1)"

want "probe reflects the REAL server: session cockpit MISSING" \
     "session cockpit" "$out"
if [[ "$out" == *"session cockpit"*"MISSING"* ]]; then
    ok "probe reports the real server's missing cockpit session, not the decoy's"
else
    bad "probe reports the real server's missing cockpit session, not the decoy's" "$out"
fi

echo
tl_summary
