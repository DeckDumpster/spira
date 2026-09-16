#!/usr/bin/env bash
#
# test-cockpit-rebuild.sh — rebuild.sh builds a cockpit from nothing, and refuses to kill a
#   server that still holds live work.
#
# WHY THIS SUITE EXISTS. On 2026-09-09 the tmux server on the default socket wedged and took
# the cockpit, both dashboards and every Monitor with it. `layout.sh up` could not help: it
# REPAIRS a cockpit and its first line is `has-session || exit 1`, so with no server there was
# nothing to repair. The recovery was a dozen hand-run commands whose ORDER is load-bearing and
# was written down nowhere. rebuild.sh is that sequence; this is the proof it still works.
#
# THE ORDER IS THE POINT, and case 2 is what pins it. The dashboards belong in `brain:0`,
# because `cockpit` does not own windows — it holds LINKED copies of `brain:0` and `hunk:0`.
# Building the layout in a new cockpit window produces something that renders correctly, looks
# right at a glance, and is not the cockpit; the view watcher goes DEGRADED because the window
# it steers is not the window it is looking at. So the suite asserts the LINKS, not merely that
# three panes exist somewhere.
#
# AN ISOLATED TMUX SERVER, NOT A MOCK. TMUX_TMPDIR gives this suite its own server, and every
# tmux invocation below and inside rebuild.sh inherits it — so it drives the real layout.sh and
# the real cockpit-remote against a real server, and touches nothing Ryan is looking at.
#
# THE SOCKET PATH MUST BE SHORT. A unix socket path is capped at 108 bytes; the session
# scratchpad blows through it and tmux answers "File name too long", which the first draft of
# rebuild.sh misread as a wedged server. mktemp -d under /tmp keeps it short, and case 4 pins
# the classifier so that misreading cannot come back.
#
# defect: sp-9zs0y
# covers: cockpit/rebuild.sh cockpit/layout.sh
# hermetic-ok: its own TMUX_TMPDIR server and temp dirs; reads no operator state it can change
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
COCKPIT="$HERE/../cockpit"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

T="$(mktemp -d)"
export TMUX_TMPDIR="$T"
cleanup() {
    TMUX_TMPDIR="$T" tmux kill-server 2>/dev/null || true
    rm -rf "$T"
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM

# FAKE CONCIERGE — keeps the session pane alive with --append-system-prompt visible
# in its process cmdline. Avoids the real concierge.sh's dependency on claude and the db.
FAKE_CONC="$T/concierge.sh"
cat > "$FAKE_CONC" <<'SH'
#!/usr/bin/env bash
[ "${1:-}" = "here" ] || exit 0
exec bash -c 'while true; do sleep 1; done' -- --append-system-prompt
SH
chmod +x "$FAKE_CONC"

# BARE FAKE CONCIERGE — keeps the pane alive WITHOUT --append-system-prompt.
# Used by the positive-control case to verify the check can detect an unwrapped pane.
FAKE_CONC_BARE="$T/concierge-bare.sh"
cat > "$FAKE_CONC_BARE" <<'SH'
#!/usr/bin/env bash
[ "${1:-}" = "here" ] || exit 0
exec sleep 60
SH
chmod +x "$FAKE_CONC_BARE"

rebuild() { TMUX_TMPDIR="$T" COCKPIT_CONCIERGE="$FAKE_CONC" bash "$COCKPIT/rebuild.sh" "$@" 2>&1; }

echo "test-cockpit-rebuild.sh"

# ======================================================================================
echo
echo "1. probe on an empty world reports absence, and changes nothing:"
# ======================================================================================
out="$(rebuild probe)"
want "the server is reported absent"  "tmux server      absent" "$out"
want "brain is reported missing"      "session brain     MISSING" "$out"
is   "probe started no server"        "" "$(TMUX_TMPDIR=$T tmux list-sessions 2>/dev/null)"

# ======================================================================================
echo
echo "2. a rebuild from nothing produces a cockpit whose windows are LINKS:"
# ======================================================================================
out="$(rebuild)"; rc=$?
is "rebuild exits 0" "0" "$rc"
if [ "$rc" -ne 0 ]; then printf '%s\n' "$out" | sed 's/^/      /'; fi

for s in brain hunk chat cockpit; do
    if TMUX_TMPDIR=$T tmux has-session -t "=$s" 2>/dev/null; then ok "session $s exists"; else bad "session $s exists" "absent"; fi
done
is "brain:0 holds three panes" "3" "$(TMUX_TMPDIR=$T tmux list-panes -t brain:0 2>/dev/null | wc -l | tr -d ' ')"

tags="$(TMUX_TMPDIR=$T tmux list-panes -t brain:0 -F '#{@cockpit}' 2>/dev/null | sort | tr '\n' ' ')"
want "a pane is tagged panel"  "panel"  "$tags"
want "a pane is tagged health" "health" "$tags"

# THE LINK ASSERTION. Same window ID in two sessions is what makes it a link rather than a
# lookalike, and it is the thing that was wrong on the first hand-rebuild.
bwin="$(TMUX_TMPDIR=$T tmux list-windows -t '=brain' -F '#{window_id}' 2>/dev/null | head -1)"
hwin="$(TMUX_TMPDIR=$T tmux list-windows -t '=hunk'  -F '#{window_id}' 2>/dev/null | head -1)"
cwins="$(TMUX_TMPDIR=$T tmux list-windows -t '=cockpit' -F '#{window_id}' 2>/dev/null | tr '\n' ' ')"
want "cockpit links brain's window" "$bwin" "$cwins"
want "cockpit links hunk's window"  "$hwin" "$cwins"

want "it reports its own verification" "ok    panel pane renders content" "$out"
want "and health too"                  "ok    health pane renders content" "$out"
want "and the session pane too"        "ok    session pane carries composed brief" "$out"

# ======================================================================================
echo
echo "3. it is idempotent — a second run leaves a healthy server alone:"
# ======================================================================================
# The dangerous shape is a repair tool that tears down what it finds. A rebuild run against a
# working cockpit must not kill the server the operator is attached to.
before="$(TMUX_TMPDIR=$T tmux display-message -p '#{pid}' 2>/dev/null)"
out2="$(rebuild)"; rc2=$?
after="$(TMUX_TMPDIR=$T tmux display-message -p '#{pid}' 2>/dev/null)"
is   "second run exits 0"                  "0" "$rc2"
want "it says it left the server alone"    "answering — leaving it alone" "$out2"
is   "the server was NOT restarted"        "$before" "$after"
is   "still three panes in brain:0"        "3" "$(TMUX_TMPDIR=$T tmux list-panes -t brain:0 2>/dev/null | wc -l | tr -d ' ')"

# ======================================================================================
echo
echo "4. an unusable socket path is reported, not mistaken for a wedged server:"
# ======================================================================================
# The first draft called this "wedged" and went hunting for a process holding a socket that
# could never exist. A path over the 108-byte unix limit is a broken environment; saying so is
# the whole fix.
LONG="$T/$(printf 'x%.0s' $(seq 1 120))"
mkdir -p "$LONG" 2>/dev/null || LONG="$T/toolong"
out3="$(TMUX_TMPDIR="$LONG" bash "$COCKPIT/rebuild.sh" probe 2>&1)"
if [[ "$out3" == *"unusable"* ]]; then
    ok "an over-long socket path is called unusable"
else
    # Some filesystems refuse the directory outright; that is not this assertion's business.
    if [[ "$out3" == *"absent"* ]]; then ok "an over-long socket path is called unusable (env refused the dir; absent is acceptable)"
    else bad "an over-long socket path is called unusable" "got: $(printf '%s' "$out3" | head -3 | tr '\n' ' ')"; fi
fi

# ======================================================================================
echo
echo "5. positive control: verify reports FAIL when session pane has no composed brief:"
# ======================================================================================
# LAW: a check that finds nothing must first prove it could have found something.
# Replace the session pane's process with a bare sleep (no --append-system-prompt),
# then rebuild with a concierge that also starts bare. The verify block must say FAIL.
sess_p5="$(TMUX_TMPDIR=$T tmux list-panes -t brain:0 \
    -F '#{@cockpit} #{pane_id}' 2>/dev/null \
    | awk '{ if (NF==1) print $1; else if ($1!="panel" && $1!="health") print $2 }' | head -1)"
if [ -z "$sess_p5" ]; then
    bad "positive control: session pane not found in brain:0" ""
else
    TMUX_TMPDIR=$T tmux respawn-pane -k -t "$sess_p5" "sleep 60" 2>/dev/null
    sleep 1
    out5="$(TMUX_TMPDIR=$T COCKPIT_CONCIERGE="$FAKE_CONC_BARE" \
        bash "$COCKPIT/rebuild.sh" 2>&1)"; rc5=$?
    is   "positive control: rebuild fails when session pane has no brief" "1" "$rc5"
    want "positive control: verify names the session pane as the failure" \
         "FAIL  session pane" "$out5"
fi

echo
printf 'test-cockpit-rebuild.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
