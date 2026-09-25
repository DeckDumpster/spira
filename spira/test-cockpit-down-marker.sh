#!/usr/bin/env bash
#
# test-cockpit-down-marker.sh — ensure tells a crash from a deliberate `down`.
#
# WHY THIS SUITE EXISTS. Zero `@cockpit` panes anywhere is what `layout.sh down` leaves
# behind, and it is also what every session crashing leaves behind — `cockpit_windows()`
# cannot see the difference, because both states are "nothing tagged, anywhere". Before this
# fix `ensure` therefore did nothing in EITHER case: a deliberate `down` correctly stayed
# down, but a crash also stayed down, silently, for as long as nobody noticed the missing
# dashboards by eye. `down` now drops a marker (`DOWN_MARKER`) and `ensure` rebuilds from
# nothing only when that marker is absent.
#
# AN ISOLATED TMUX SERVER PER CASE, NOT A MOCK. Each case gets its own TMUX_TMPDIR, so this
# suite drives the real layout.sh and rebuild.sh against a real server and touches nothing
# the operator is looking at.
#
# defect: sp-tc9ha
# covers: cockpit/layout.sh cockpit/rebuild.sh
# hermetic-ok: its own TMUX_TMPDIR servers and temp dirs; reads no operator state it can change
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
COCKPIT_DIR="$HERE/../cockpit"
LAYOUT="$COCKPIT_DIR/layout.sh"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
hasnt(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T1="$(mktemp -d)"; T2="$(mktemp -d)"
cleanup() {
    TMUX_TMPDIR="$T1" tmux kill-server 2>/dev/null || true
    TMUX_TMPDIR="$T2" tmux kill-server 2>/dev/null || true
    rm -rf "$T1" "$T2"
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM

# A fake concierge, same trick as test-cockpit-rebuild.sh: keeps the session pane alive
# with --append-system-prompt visible in its cmdline, without depending on claude or a db.
FAKE_CONC="$T1/concierge.sh"
cat > "$FAKE_CONC" <<'SH'
#!/usr/bin/env bash
[ "${1:-}" = "here" ] || exit 0
exec bash -c 'while true; do sleep 1; done' -- --append-system-prompt
SH
chmod +x "$FAKE_CONC"

echo "test-cockpit-down-marker.sh"

# ======================================================================================
echo
echo "1. down, then ensure: the marker is written and ensure leaves it down"
# ======================================================================================
RUN1="$T1/run"; mkdir -p "$RUN1"
export TMUX_TMPDIR="$T1"
tmux start-server
tmux new-session -d -s brain -x 214 -y 53

layout1() {
    SPIRA_COCKPIT="$COCKPIT_DIR" SPIRA_REPO="$T1" SPIRA_RUN="$RUN1" SPIRA_HOME="$HERE" \
    SPIRA_CONF="$T1/no.conf" COCKPIT_CONCIERGE="$FAKE_CONC" COCKPIT_MAIL="" \
        bash "$LAYOUT" "$@" 2>&1
}

out="$(layout1 up --window brain:0)"; rc=$?
is "up exits 0" "0" "$rc"
if [ "$rc" -ne 0 ]; then printf '%s\n' "$out" | sed 's/^/      /'; fi
tags="$(tmux list-panes -t brain:0 -F '#{@cockpit}' 2>/dev/null | sort | tr '\n' ' ')"
want "up: a pane is tagged health" "health" "$tags"

dash_tags() { tmux list-panes -t brain:0 -F '#{@cockpit}' 2>/dev/null | grep -cE '^(health|mail)$' || true; }

out="$(layout1 down --window brain:0)"; rc=$?
is "down exits 0" "0" "$rc"
is "down: DOWN_MARKER exists" "1" \
   "$([ -f "$RUN1/cockpit.down" ] && echo 1 || echo 0)"
is "down: no dashboard panes remain" "0" "$(dash_tags)"

out="$(layout1 ensure)"; rc=$?
is "ensure exits 0" "0" "$rc"
is "ensure after down: still no dashboard panes" "0" "$(dash_tags)"
is "ensure after down: DOWN_MARKER still there" "1" \
   "$([ -f "$RUN1/cockpit.down" ] && echo 1 || echo 0)"
if [ -f "$RUN1/cockpit-heal.log" ]; then
    hasnt "ensure after down: heal.log does not claim a rebuild" \
          "rebuilding from scratch" "$(cat "$RUN1/cockpit-heal.log")"
else
    ok "ensure after down: no heal.log written at all"
fi
is "ensure after down: session brain is untouched" "1" \
   "$(tmux has-session -t '=brain' 2>/dev/null && echo 1 || echo 0)"

# ======================================================================================
echo
echo "2. every session gone with NO marker: ensure rebuilds from scratch"
# ======================================================================================
RUN2="$T2/run"; mkdir -p "$RUN2"
TMUX_TMPDIR="$T2" tmux start-server

layout2() {
    SPIRA_COCKPIT="$COCKPIT_DIR" SPIRA_REPO="$T2" SPIRA_RUN="$RUN2" SPIRA_HOME="$HERE" \
    SPIRA_CONF="$T2/no.conf" COCKPIT_CONCIERGE="$FAKE_CONC" COCKPIT_MAIL="" \
    TMUX_TMPDIR="$T2" \
        bash "$LAYOUT" "$@" 2>&1
}

is "2: no cockpit.down marker before ensure" "0" \
   "$([ -f "$RUN2/cockpit.down" ] && echo 1 || echo 0)"
is "2: no sessions exist before ensure (the crash)" "" \
   "$(TMUX_TMPDIR="$T2" tmux list-sessions 2>/dev/null)"

out="$(layout2 ensure)"; rc=$?
is "2: ensure exits 0" "0" "$rc"

for s in brain hunk chat cockpit; do
    if TMUX_TMPDIR="$T2" tmux has-session -t "=$s" 2>/dev/null; then
        ok "2: session $s was rebuilt"
    else
        bad "2: session $s was rebuilt" "absent"
    fi
done
tags2="$(TMUX_TMPDIR="$T2" tmux list-panes -t brain:0 -F '#{@cockpit}' 2>/dev/null | sort | tr '\n' ' ')"
want "2: a pane is tagged health after the rebuild" "health" "$tags2"
want "2: heal.log records the rebuild" \
     "rebuilding from scratch" "$(cat "$RUN2/cockpit-heal.log" 2>/dev/null)"

echo
printf 'test-cockpit-down-marker.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
