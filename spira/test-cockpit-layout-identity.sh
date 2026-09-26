#!/usr/bin/env bash
#
# test-cockpit-layout-identity.sh — ensure never tags, and never kills, a pane that merely
# MENTIONS a dashboard.
#
# THE FAILURE THIS SUITE EXISTS FOR. layout.sh identified a dashboard pane by globbing
# `*cockpit/health.sh*` over the joined argv of the pane's processes. An agent session whose
# system prompt lists `cockpit/health.sh` as a tool matched, was tagged `health`, and the next
# ensure found no untagged pane, declared the session pane gone, and ran `up` — which kills
# every tagged pane. The live session died every other minute.
#
# Fixture: a throwaway tmux server holding one window with
#   - a "session" pane whose process carries the dashboard path inside an argument, and
#   - a real health dashboard pane running `bash <fixture>/cockpit/health.sh loop`.
# The real one is the positive control: a matcher that tags nothing would also pass the
# "session stays untagged" assertion.
#
# tier: T1
# covers: cockpit/layout.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
COCKPIT_DIR="$(dirname "$HERE")/cockpit"
LAYOUT="$COCKPIT_DIR/layout.sh"


command -v tmux >/dev/null 2>&1 || { echo "SKIP: tmux not available" >&2; exit 0; }

TMP="$(mktemp -d)"
TMUXDIR="$TMP/tmux-fixture"; mkdir -p "$TMUXDIR"
export TMUX_TMPDIR="$TMUXDIR"
unset TMUX TMUX_PANE
cleanup() { tmux kill-server 2>/dev/null || true; rm -rf "$TMP"; }
trap cleanup EXIT

mkdir -p "$TMP/.runtime" "$TMP/fake/cockpit"
# A stand-in dashboard: same path SHAPE as the real one, and it only sleeps, so the fixture
# never runs the real health loop against the real store.
# No `exec`: bash must stay the process so argv[1] is the script path, as in a real pane.
printf '#!/usr/bin/env bash\nsleep 600\n' > "$TMP/fake/cockpit/health.sh"
chmod +x "$TMP/fake/cockpit/health.sh"

# The impostor: an ordinary process whose ARGUMENTS mention the dashboard, as an agent's
# --append-system-prompt does. The mention rides in bash's $0 slot so the pane stays alive
# (an unknown extra argument to `sleep` itself would exit at once and take the server with it).
MENTION="tools: $COCKPIT_DIR/health.sh once and cockpit/health.sh loop"
tmux new-session -d -s cockpit -x 200 -y 50 "bash -c 'sleep 600 & wait' '$MENTION'"
SESS=$(tmux list-panes -t cockpit:0 -F '#{pane_id}' | head -1)
HEALTH=$(tmux split-window -P -F '#{pane_id}' -d -h -t "$SESS" "bash $TMP/fake/cockpit/health.sh loop")
sleep 0.5

# Prove the impostor really carries the string the old matcher looked for, or the suite would
# pass against a fixture that could never have tripped it.
spid=$(tmux display -p -t "$SESS" '#{pane_pid}' 2>/dev/null)
if [ -z "$spid" ] || [ -z "$HEALTH" ]; then
    echo "fixture: tmux server did not hold the panes (session=[$SESS] health=[$HEALTH])" >&2
    exit 1
fi
cl="$(tr '\0' ' ' </proc/"$spid"/cmdline 2>/dev/null)"
for c in $(pgrep -P "$spid"); do cl="$cl $(tr '\0' ' ' </proc/"$c"/cmdline 2>/dev/null)"; done
[[ "$cl" == *cockpit/health.sh* ]] && ok "fixture: session pane argv mentions cockpit/health.sh" \
    || bad "fixture: session pane argv mentions cockpit/health.sh" "argv was [$cl]"

# Pre-poison the session pane's tag, as a previous (buggy) pass left it.
tmux set-option -p -t "$SESS" @cockpit health

SPIRA_COCKPIT="$COCKPIT_DIR" \
SPIRA_REPO="$TMP" \
SPIRA_RUN="$TMP/.runtime" \
SPIRA_HOME="$HERE" \
SPIRA_CONF="$TMP/no.conf" \
SPIRA_PANEL="$TMP/fake/nonexistent-panel" \
COCKPIT_CLIENT_IDLE_SECS=0 \
    bash "$LAYOUT" ensure --window cockpit:0 >/dev/null 2>&1 || true
sleep 0.3

alive=$(tmux list-panes -t cockpit:0 -F '#{pane_id}' 2>/dev/null | grep -Fxc "$SESS")
is "session pane survives ensure" "1" "$alive"
is "session pane is untagged after ensure" "" "$(tmux display -p -t "$SESS" '#{@cockpit}' 2>/dev/null)"
is "positive control: real health pane is tagged health" "health" \
    "$(tmux display -p -t "$HEALTH" '#{@cockpit}' 2>/dev/null)"
grep -q "runs no dashboard" "$TMP/.runtime/cockpit-heal.log" 2>/dev/null \
    && ok "heal log records the cleared tag" \
    || bad "heal log records the cleared tag" "$(cat "$TMP/.runtime/cockpit-heal.log" 2>/dev/null)"
tl_summary
