#!/usr/bin/env bash
#
# test-concierge-acceptance.sh — the concierge behaviours that only a real systemd user
#   session and a real tmux can exercise: the oneshot's cgroup teardown, the launcher's
#   --resume flag under systemd-run, the dangling-resume retry, and the cockpit layout pane.
#
# HOST-ONLY, AND SAID SO (G-12, UC-operator-channel-40's own tier: "cgroup survival and
# layout pane T4/acc (host)"). A container running only the harness has neither a systemd
# user session nor (usually) tmux wired for it; before this file existed, each case below
# printed its own ad hoc "skip" line and exited 0, so a container run and an operator-host
# run reported the same "self-test: N passed, 0 failed" for a very different N. One `skip`
# call at the top makes the whole file a single, counted TAP skip (exit 77) instead —
# reported as exactly what it is.
#
# D10 (within-file duplicates): the original test-concierge.sh carried two near-identical
# "launcher carries --resume" sections under different headings (one framed as "resume",
# one as "cockpit.sh attaches the operator" though neither called cockpit.sh). Merged into
# one case here, keeping the richer assertion set (the SPIRA_CONCIERGE=1 export check).
#
# defect: sp-u4x
# covers: concierge.sh systemd/concierge.service cockpit/layout.sh UC-operator-channel-40
# requires: claude
# host-reason: needs a real systemd --user session (systemd-run --wait), tmux, and the cockpit layout script; not available in the container
# tier: T4
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
HARNESS="$(cd "$HERE/.." && pwd)"
. "$HERE/testlib.sh"

if ! systemctl --user status >/dev/null 2>&1 || ! command -v systemd-run >/dev/null 2>&1 \
   || ! command -v tmux >/dev/null 2>&1; then
    skip "no systemd --user session or no tmux — this is a T4 host acceptance suite, reported as skipped rather than folded into a container's green"
fi
if ! bash "$HARNESS/rule.sh" list 2>/dev/null | grep -q .; then
    skip "the statute book is empty on this host — brief composition would fail for a reason unrelated to what this file tests"
fi

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "start — session survives the oneshot's cgroup teardown"

# THE PROPERTY UNDER TEST. concierge.service is Type=oneshot/KillMode=control-group: the
# server shared the service's cgroup and died when start exited. We simulate the oneshot
# with systemd-run --wait (which kills its cgroup on exit) and assert the session still
# exists. Without the fix, the session dies with the cgroup.
SOCK="test-concierge-$$"
tmux -L "$SOCK" kill-server 2>/dev/null || true

rc_start=0
systemd-run --user --wait --collect --quiet -- \
    env CONCIERGE_SOCKET="$SOCK" CONCIERGE_SESSION="$SOCK" \
    bash "$HARNESS/concierge.sh" start 2>>"$TMP/err" || rc_start=$?

is "start exits 0 under a simulated oneshot" 0 "$rc_start"

alive=0
tmux -L "$SOCK" has-session -t "$SOCK" 2>/dev/null && alive=1
is "session survives after the oneshot cgroup is torn down" 1 "$alive"

tmux -L "$SOCK" kill-server 2>/dev/null || true  # drains the nested transient unit

echo
echo "resume — the launcher carries --resume exactly when a matching session is recorded"

# POSITIVE CONTROL: a launcher with no session file MUST NOT carry --resume, so if the check
# were absent the "resume present" assertion below would pass against a launcher that adds
# it unconditionally.
TMP_RUN="$TMP/run-resume"; mkdir -p "$TMP_RUN"
FAKE_SID_R="resume-launcher-test-$(date +%s)"

rm -f "$TMP_RUN/concierge-session"
SOCK_NR="test-concierge-noresume-$$"
tmux -L "$SOCK_NR" kill-server 2>/dev/null || true
systemd-run --user --wait --collect --quiet -- \
    env SPIRA_RUN="$TMP_RUN" CONCIERGE_SOCKET="$SOCK_NR" CONCIERGE_SESSION="$SOCK_NR" \
    bash "$HARNESS/concierge.sh" start 2>>"$TMP/err" || true
tmux -L "$SOCK_NR" kill-server 2>/dev/null || true
if [ -f "$TMP_RUN/concierge-launch.sh" ]; then
    nowant "launcher has no --resume when no session file" "--resume" \
        "$(cat "$TMP_RUN/concierge-launch.sh")"
else
    bad "start wrote a launcher (no-session case)" "no launcher file"
fi

# THE PROPERTY: with a session file whose recorded cwd matches BRAIN, the new launcher
# carries --resume <id>, and exports SPIRA_CONCIERGE=1 the same as any other start.
printf '%s\n%s\n' "$FAKE_SID_R" "$TMP_RUN" > "$TMP_RUN/concierge-session"
SOCK_R="test-concierge-resume2-$$"
tmux -L "$SOCK_R" kill-server 2>/dev/null || true
systemd-run --user --wait --collect --quiet -- \
    env SPIRA_RUN="$TMP_RUN" SPIRA_WIKI="$TMP_RUN" \
        CONCIERGE_SOCKET="$SOCK_R" CONCIERGE_SESSION="$SOCK_R" \
    bash "$HARNESS/concierge.sh" start 2>>"$TMP/err" || true
tmux -L "$SOCK_R" kill-server 2>/dev/null || true
if [ -f "$TMP_RUN/concierge-launch.sh" ]; then
    lnch="$(cat "$TMP_RUN/concierge-launch.sh")"
    want "launcher carries --resume when a matching session file exists" "--resume"    "$lnch"
    want "and names the specific recorded session id"                    "$FAKE_SID_R" "$lnch"
    want "and the launcher exports SPIRA_CONCIERGE=1"                    "SPIRA_CONCIERGE=1" "$lnch"
else
    bad "start wrote a launcher (resume case)" "no launcher file"
fi

echo
echo "the dangling-resume retry: --resume fails, start retries fresh"

# SEEN TO FAIL FIRST (code-shape checks for this live in test-concierge.sh). Here the
# behaviour runs for real: a `claude` stub that fails on --resume and stays up otherwise.
_ddir="$(mktemp -d)"; mkdir -p "$_ddir/bin"
_dfake_id="dangling-$$-$(date +%s)"
cat > "$_ddir/bin/claude" <<'STUB'
#!/bin/sh
case " $* " in *' --resume '*) exit 1 ;; esac
exec sleep 30
STUB
chmod +x "$_ddir/bin/claude"
printf '%s\n%s\n' "$_dfake_id" "$_ddir" > "$_ddir/concierge-session"
_dsock="test-concierge-dangle-$$"
tmux -L "$_dsock" kill-server 2>/dev/null || true
_dout="$(PATH="$_ddir/bin:$PATH" SPIRA_RUN="$_ddir" SPIRA_WIKI="$_ddir" \
         CONCIERGE_SOCKET="$_dsock" CONCIERGE_SESSION="$_dsock" \
         bash "$HARNESS/concierge.sh" start 2>&1)"; _drc=$?
_dsess_after="$(cat "$_ddir/concierge-session" 2>/dev/null)"
tmux -L "$_dsock" kill-server 2>/dev/null || true
rm -rf "$_ddir"
want "dangling resume triggers retry"  "retrying without it" "$_dout"
want "retry reports a FRESH start"     "FRESH"               "$_dout"
is   "session file is cleared"         ""                    "$_dsess_after"
is   "start exits 0 after fresh retry" 0                     "$_drc"

echo
echo "cockpit.sh attaches the operator: layout.sh up builds the session pane from concierge.sh here"

# SEEN TO FAIL FIRST: before this change, layout.sh up created a session pane with a plain
# shell (no explicit command). After, the pane command contains concierge.sh here.
#
# FIXTURE DESIGN. layout.sh uses bare `tmux` (no -L), so all bare calls are redirected to
# the fixture via TMUX_TMPDIR. We start health.sh loop in the only pane so pane_role()
# identifies it as health — a manually-forced tag would be cleared by retag_dashboards.
# With all panes tagged, session_pane() returns empty and up creates the concierge pane.
LAYOUT_SH="$HARNESS/cockpit/layout.sh"
if [ ! -x "$LAYOUT_SH" ]; then
    bad "layout.sh present" "not found at $LAYOUT_SH"
else
    LTMP="$TMP/layout-d"; mkdir -p "$LTMP"
    LDIR="$LTMP/tmux"; mkdir -p "$LDIR"
    LSESS="cockpit-d"
    LCONF="$LTMP/spira.conf"
    printf 'SPIRA_PROD = %s\nSPIRA_RUN = %s\n' "$HARNESS" "$LTMP/run" > "$LCONF"
    mkdir -p "$LTMP/run"

    TMUX_TMPDIR="$LDIR" tmux start-server
    TMUX_TMPDIR="$LDIR" tmux new-session -d -s "$LSESS" -x 200 -y 50

    FIRST_PANE=$(TMUX_TMPDIR="$LDIR" tmux list-panes -t "$LSESS" -F '#{pane_id}')
    TMUX_TMPDIR="$LDIR" tmux respawn-pane -k -t "$FIRST_PANE" \
        "SPIRA_RUN=$LTMP/run SPIRA_CONF=$LCONF bash $HARNESS/cockpit/health.sh loop"
    sleep 0.5

    TMUX="" TMUX_TMPDIR="$LDIR" SPIRA_CONF="$LCONF" SPIRA_REPO="$HARNESS" \
        COCKPIT_RIGHT_PCT=33 COCKPIT_BOTTOM_PCT=30 COCKPIT_CWD="$LTMP" \
        COCKPIT_MAIL="" COCKPIT_MOUSE=off COCKPIT_CLIENT_IDLE_SECS=0 \
        bash "$LAYOUT_SH" up --window "$LSESS:0" 2>/dev/null || true

    new_cmd=""
    while IFS='|' read -r tag pid; do
        [ -z "$tag" ] || continue
        new_cmd="$(TMUX_TMPDIR="$LDIR" tmux list-panes -t "$LSESS:0" \
            -F '#{@cockpit}|#{pane_id}|#{pane_start_command}' 2>/dev/null \
            | awk -F'|' -v p="$pid" '$2==p{print $3}')"
        break
    done < <(TMUX_TMPDIR="$LDIR" tmux list-panes -t "$LSESS:0" \
             -F '#{@cockpit}|#{pane_id}' 2>/dev/null \
             | awk -F'|' '$1==""')

    want "session pane is started with concierge.sh here" "concierge.sh here" "${new_cmd:-}"

    TMUX_TMPDIR="$LDIR" tmux kill-server 2>/dev/null || true
fi

tl_summary
