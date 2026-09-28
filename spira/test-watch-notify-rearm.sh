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
# STATUS: checkpoint 1 of 3 (sp-bz7uh.4). This commit is the container-harness skeleton only,
# copied from test-cadence-tool.sh — TODO markers below mark where the real
# spira-watch-notify.timer install + POSITIVE CONTROL + NextElapseUSec* assertions land in
# checkpoints 2 and 3 (sp-bz7uh.5, sp-bz7uh.6). No podman run has been exercised yet.
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

TESTENV="$HERE/testenv.sh"
CNAME="spira-testenv-watchnotify-$$"

cleanup() {
    bash "$TESTENV" down --name "$CNAME" --volumes >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

bash "$TESTENV" up --name "$CNAME" >&2
iszero "container up exits 0" "$?"

if ! bash "$TESTENV" probe --name "$CNAME"; then
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

# TODO (sp-bz7uh.5), IN PROGRESS — two things confirmed manually against this harness's own
# container, neither yet folded into the script:
#
#   (a) RAW UNIT FILES DO NOT LOAD. systemd/spira-watch-notify.service's ExecStart is a
#       template (@SPIRA_PROD@/watchd.sh, see systemd/install.sh) — podman-cp'd verbatim,
#       systemd refuses it: "Neither a valid executable name nor an absolute path", the
#       service ends up "bad-setting", and the timer then refuses to start ("unit ... to
#       trigger not loaded"). Render @SPIRA_HOME@/@SPIRA_PROD@ to any absolute dummy path
#       (sed, or reuse install.sh's renderer) before installing either unit here.
#
#   (b) A FRESH INSTALL DOES NOT REPRODUCE sp-0djeb'S BUG. Rendering the templates, then
#       daemon-reload + restart on a never-before-started OnActiveSec-stripped timer gives
#       NextElapseUSecMonotonic populated (from OnBootSec/OnUnitActiveSec) and
#       ActiveState=active/waiting — NOT the "active (elapsed)"-with-both-empty state
#       sp-0djeb saw. That bug followed a daemon-reload + restart of a timer that had
#       ALREADY been running for a while (its boot/active anchors already consumed) — a
#       fresh install skips that history. Next attempt: install the UNSTRIPPED timer,
#       start it, `systemctl --user stop`, THEN swap in the stripped copy, daemon-reload,
#       restart, and check Next* again. Do not write the SEEN RED assertion until that
#       sequence is confirmed to actually go red (law-a-regression-test-must-be-seen-to-fail).
#
# See sp-bz7uh.5's bead notes for the full transcript of both checks.
#
# TODO (sp-bz7uh.6): against the real (fixed) timer, assert
#   systemctl --user show spira-watch-notify.timer \
#       -p NextElapseUSecRealtime -p NextElapseUSecMonotonic
# reports at least one of the two as nonempty/nonzero after daemon-reload + restart.

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
