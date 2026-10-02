#!/usr/bin/env bash
#
# test-watch-notify-rearm.sh — spira-watch-notify.timer re-arms after daemon-reload + restart.
#
#   ./test-watch-notify-rearm.sh
#
# WHAT THIS TESTS (sp-n0c9d). sp-0djeb described spira-watch-notify.timer going
# "active (elapsed)" with no next trigger after a daemon-reload + restart with no reboot —
# systemd left it with neither NextElapseUSecRealtime nor NextElapseUSecMonotonic set.
# sp-0djeb's fix (OnActiveSec=5min in systemd/spira-watch-notify.timer, commit 5e2a35f54 via
# sp-ly6l9) is confirmed landed on main; test-units-lint.sh's line-presence/ratio assertions
# pass unmodified and never exercise the actual failure. This suite renders the REAL unit
# under a live systemd --user instance and asserts it re-arms.
#
# THIS SUITE RUNS INSIDE A CONTAINER WITH REAL SYSTEMD (law-prefer-the-real-dependency), same
# model as spira/test-cadence-tool.sh: the failure is a property of how systemd populates
# NextElapseUSecRealtime/Monotonic, which a hand-written systemctl stub cannot reproduce
# faithfully.
#
# HOST ISOLATION. The container runs its own user systemd; scratch units are created inside
# the container and removed in a trap. The host's ~/.config/systemd/user is snapshotted
# before and after; any change fails the test.
#
# STATUS: checkpoint 2 of 3. The positive control is in; the fixed-timer assertion lands in
# checkpoint 3 (sp-bz7uh.6).
#
# tier: T1
# covers: systemd/spira-watch-notify.timer
# priority: 2
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

hasnt()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "found [$2] in [$3]"; }
iszero() { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }

echo "test-watch-notify-rearm.sh"

command -v podman >/dev/null 2>&1 || skip "podman not on PATH"

CNAME="spira-testenv-watchnotify-$$"

cleanup() {
    testenv container down --name "$CNAME" --volumes >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

testenv container up --name "$CNAME" >&2
iszero "container up exits 0" "$?"

if ! testenv container probe --name "$CNAME"; then
    bad "user systemd running in container" "probe failed"
    tl_summary
    exit
fi
ok "user systemd running in container"

# HOST SNAPSHOT — before any container work, so a diff after proves no host contamination.
snap_before="$(ls -1 "$HOME/.config/systemd/user/" 2>/dev/null | sort || true)"

CEXEC=(podman exec --user spirauser
    -e XDG_RUNTIME_DIR=/run/user/1001
    -e "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1001/bus"
)
SC=(podman exec --user spirauser
    -e XDG_RUNTIME_DIR=/run/user/1001
    -e "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1001/bus"
    "$CNAME" systemctl --user)

# UDIR is computed from the container user's $HOME at runtime so no home path is
# written into this source file (inventory.sh refuses shipped home-directory literals).
UDIR="$("${CEXEC[@]}" "$CNAME" bash -c 'printf "%s/.config/systemd/user" "$HOME"')"

# Units are rendered to a dummy absolute path: raw templates do not load. Intervals are
# shrunk to 5s; the bug needs a timer that has already fired, so each variant is started,
# allowed to fire, then daemon-reloaded and restarted.
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"; cleanup' EXIT INT TERM
sed -e 's#@[A-Za-z_]*@#/opt/x#g' -e 's#^ExecStart=.*#ExecStart=/bin/true#' \
    -e '/^Standard\(Output\|Error\)=/d' "$HERE/../systemd/spira-watch-notify.service" \
    > "$WORK/spira-watch-notify.service"
sed -e 's/=5min$/=5s/' "$HERE/../systemd/spira-watch-notify.timer" > "$WORK/fixed.timer"
grep -v '^OnActiveSec=' "$WORK/fixed.timer" > "$WORK/stripped.timer"

# trigger_after_restart <timer file>: prints the `Trigger:` line after fire + reload + restart.
trigger_after_restart() {
    "${CEXEC[@]}" "$CNAME" mkdir -p "$UDIR"
    podman cp "$WORK/spira-watch-notify.service" "$CNAME:$UDIR/spira-watch-notify.service"
    podman cp "$1" "$CNAME:$UDIR/spira-watch-notify.timer"
    podman exec --user root "$CNAME" chown -R spirauser "$UDIR"
    "${SC[@]}" daemon-reload
    "${SC[@]}" stop spira-watch-notify.timer
    "${SC[@]}" start spira-watch-notify.timer
    sleep 8
    "${SC[@]}" daemon-reload
    "${SC[@]}" restart spira-watch-notify.timer
    sleep 1
    "${SC[@]}" status spira-watch-notify.timer --no-pager | grep 'Trigger:'
}

echo
echo "positive control — the OnActiveSec-stripped timer is seen to go red"
red="$(trigger_after_restart "$WORK/stripped.timer")"
case "$red" in *"Trigger: n/a"*) ok "stripped timer: Trigger: n/a after reload+restart (SEEN RED)";;
    *) bad "stripped timer reproduces sp-0djeb" "got [$red]";; esac

echo
echo "fix path — the real (fixed) timer keeps a next elapse after fire + reload + restart"
trigger_after_restart "$WORK/fixed.timer" >/dev/null
nxt="$("${SC[@]}" show spira-watch-notify.timer -p NextElapseUSecRealtime -p NextElapseUSecMonotonic)"
nz="$(printf '%s\n' "$nxt" | awk -F= '$2!="" && $2!="0" && $2!="n/a"' | head -1)"
[ -n "$nz" ] && ok "fixed timer has a next elapse after reload+restart (SEEN GREEN)" \
    || bad "fixed timer rearms after restart" "got [$nxt]"

# ---------------------------------------------------------------------------
echo
echo "host isolation — container test writes nothing to the host unit directory"
# ---------------------------------------------------------------------------
snap_after="$(ls -1 "$HOME/.config/systemd/user/" 2>/dev/null | sort || true)"
diff_out="$(diff <(printf '%s\n' "$snap_before") <(printf '%s\n' "$snap_after") || true)"
[ -z "$diff_out" ] \
    && ok "host ~/.config/systemd/user is unchanged" \
    || bad "host unit directory changed" "$(printf '%s\n' "$diff_out" | head -10)"

echo
tl_summary
