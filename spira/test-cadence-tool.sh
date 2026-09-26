#!/usr/bin/env bash
#
# test-cadence-tool.sh — cadence.sh changes a timer's cadence, and REFUSES to disarm one.
#
#   ./test-cadence-tool.sh
#
# WHAT THIS TESTS. cadence.sh changes how often an installed systemd timer fires — without
# editing the committed template and without silently disarming the unit in the process.
# The key failure it exists to prevent: a naive override that clears OnUnitActiveSec and
# re-sets it to a new value leaves the timer with no monotonic anchor, so systemd cannot
# schedule a next elapse, so the timer silently stops firing while reporting exit 0.
#
# THIS SUITE RUNS INSIDE A CONTAINER WITH REAL SYSTEMD (law-prefer-the-real-dependency).
# The behaviour under test is entirely a property of how systemd populates
# NextElapseUSecRealtime vs. NextElapseUSecMonotonic, and which of the two is empty when the
# unit is truly disarmed. A hand-written `systemctl` stub cannot reproduce that distinction
# faithfully — both defects this suite pins were cases of the stub getting it wrong.
#
# HOST ISOLATION. The container runs its own user systemd; scratch units are created inside
# the container and removed in a trap. The host's ~/.config/systemd/user is snapshotted
# before and after; any change fails the test.
#
# POSITIVE CONTROL OPENS THE SUITE. A naive override that writes only OnUnitActiveSec=6h
# must be seen to disarm the timer before any assertion about cadence.sh is meaningful —
# otherwise the suite proves the tool prevents something that cannot happen on this platform
# (law-a-regression-test-must-be-seen-to-fail, law-absence-needs-a-positive-control).
#
# scar: the first version of this suite ran directly on the host's systemd and relied on an
# EXIT trap for cleanup; a slain shell left stray units named after a dead pid. Container
# isolation was the fix (sp-dah).
#
# tier: T1
# covers: spira/cadence.sh
# priority: 2
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

hasnt()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "found [$2] in [$3]"; }
iszero() { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }

echo "test-cadence-tool.sh"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-cadence-tool.sh: podman not found on PATH\n' >&2
    exit 77
}

TESTENV="$HERE/testenv.sh"
CNAME="spira-testenv-cadtool-$$"

cleanup() {
    bash "$TESTENV" down --name "$CNAME" --volumes >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

bash "$TESTENV" up --name "$CNAME" >&2
iszero "container up exits 0" "$?"

if ! bash "$TESTENV" probe --name "$CNAME"; then
    printf 'SKIP test-cadence-tool.sh: user systemd not available in container\n' >&2
    exit 77
fi
ok "user systemd running in container"

# HOST SNAPSHOT — before any container work, so a diff after proves no host contamination.
snap_before="$(ls -1 "$HOME/.config/systemd/user/" 2>/dev/null | sort || true)"

CEXEC=(podman exec --user spirauser
    -e XDG_RUNTIME_DIR=/run/user/1001
    -e "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1001/bus"
)
# SPIRA_RUN must be set to a writable path in the container: lib.sh runs `mkdir -p "$SPIRA_RUN"`
# at source time, and /workspace is bind-mounted with the host's ownership — spirauser cannot
# write there. SPIRA_INSTANCE must be absent or 'prod' (the default) so the containment check
# that lib.sh runs at source time is a no-op; a non-prod instance would require a configured
# repo map with all checkouts under SPIRA_WORKSPACES, which a cadence test does not need.
CADENCE=(bash /workspace/spira/cadence.sh)
SPIRA_RUN_CTR="/tmp/spira-cadtool-$$"
SC=(podman exec --user spirauser
    -e XDG_RUNTIME_DIR=/run/user/1001
    -e "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1001/bus"
    "$CNAME" systemctl --user)

# UDIR is computed from the container user's $HOME at runtime so no home path is
# written into this source file (inventory.sh refuses shipped home-directory literals).
UDIR="$("${CEXEC[@]}" "$CNAME" bash -c 'printf "%s/.config/systemd/user" "$HOME"')"
TAG="sptest_cadtool_$$"
UNIT="${TAG}.timer"
SVC="${TAG}.service"

# ---------------------------------------------------------------------------
# FIXTURE: a monotonic-only timer in the container whose OnBootSec is far in
# the future, so its service never runs and the monotonic anchor can be lost.
# This is the shape that makes the naive-override disarm reachable.
# ---------------------------------------------------------------------------
"${CEXEC[@]}" "$CNAME" bash -c "
mkdir -p '${UDIR}'
cat > '${UDIR}/${SVC}' <<'SVCEOF'
[Unit]
Description=cadence.sh test scratch unit
[Service]
Type=oneshot
ExecStart=/bin/true
SVCEOF
cat > '${UDIR}/${UNIT}' <<'TIMEREOF'
[Unit]
Description=cadence.sh test scratch timer
[Timer]
OnBootSec=9999d
OnUnitActiveSec=1h
Unit=${TAG}.service
TIMEREOF
systemctl --user daemon-reload  # hermetic-ok: inside container via podman exec
systemctl --user start '${UNIT}'  # hermetic-ok: inside container via podman exec
" >&2
iszero "scratch unit installed and started" "$?"

timer_active="$("${SC[@]}" is-active "$UNIT" 2>&1)"
want "scratch timer is active" "active" "$timer_active"

# ---------------------------------------------------------------------------
echo
echo "POSITIVE CONTROL — naive override must be seen to disarm the timer"
# Without this check every assertion below is meaningless: if the platform
# keeps the timer armed regardless, the tool is proving it prevents something
# that cannot happen (law-a-regression-test-must-be-seen-to-fail).
# ---------------------------------------------------------------------------
"${CEXEC[@]}" "$CNAME" bash -c "
mkdir -p '${UDIR}/${UNIT}.d'
printf '[Timer]\nOnUnitActiveSec=\nOnBootSec=\nOnUnitActiveSec=6h\n' \
    > '${UDIR}/${UNIT}.d/naive.conf'
systemctl --user daemon-reload  # hermetic-ok: inside container via podman exec
systemctl --user restart '${UNIT}'  # hermetic-ok: inside container via podman exec
" >&2
rt="$("${SC[@]}" show "$UNIT" -p NextElapseUSecRealtime --value 2>/dev/null)"
mono="$("${SC[@]}" show "$UNIT" -p NextElapseUSecMonotonic --value 2>/dev/null)"
if [ -z "$rt" ] && { [ "$mono" = "infinity" ] || [ -z "$mono" ]; }; then
    ok "SEEN RED: naive override leaves the timer with no next elapse"
else
    bad "SEEN RED: naive override leaves the timer with no next elapse" \
        "it stayed armed (realtime=[$rt] monotonic=[$mono]) — this box does not reproduce the scar"
fi
# Restore the fixture for subsequent tests.
"${CEXEC[@]}" "$CNAME" bash -c "
rm -rf '${UDIR}/${UNIT}.d'
systemctl --user daemon-reload  # hermetic-ok: inside container via podman exec
systemctl --user restart '${UNIT}'  # hermetic-ok: inside container via podman exec
" >&2

# ---------------------------------------------------------------------------
echo
echo "set — 6h becomes wall-clock; the timer stays armed"
# ---------------------------------------------------------------------------
out="$("${CEXEC[@]}" \
    -e "SPIRA_RUN=${SPIRA_RUN_CTR}" \
    -e "SPIRA_SYSTEMD_USER_DIR=${UDIR}" \
    "$CNAME" "${CADENCE[@]}" set "$UNIT" 6h --why "suite" 2>&1)"
want "cadence reports the new cadence"   "-> 6h" "$out"
hasnt "it does not refuse"               "REFUSED" "$out"

rt="$("${SC[@]}" show "$UNIT" -p NextElapseUSecRealtime --value 2>/dev/null)"
if [ -n "$rt" ]; then ok "timer has a real next elapse"
else bad "timer has a real next elapse" "NextElapseUSecRealtime empty — tool disarmed it"; fi

d="${UDIR}/${UNIT}.d/cadence.conf"
body="$("${CEXEC[@]}" "$CNAME" cat "$d" 2>/dev/null || true)"
want "6h becomes a wall-clock schedule"         "OnCalendar=*-*-* 0/6:00:00" "$body"
hasnt "not a bare monotonic span"               "OnUnitActiveSec=6h" "$body"
want "empty assignment clears the template"     "OnUnitActiveSec=" "$body"
want "drop-in records the reason"               "suite" "$body"
want "and how to revert"                        "cadence.sh clear" "$body"

# ---------------------------------------------------------------------------
echo
echo "set — a span with no clean calendar equivalent keeps span + backstop"
# ---------------------------------------------------------------------------
out="$("${CEXEC[@]}" \
    -e "SPIRA_RUN=${SPIRA_RUN_CTR}" \
    -e "SPIRA_SYSTEMD_USER_DIR=${UDIR}" \
    "$CNAME" "${CADENCE[@]}" set "$UNIT" 7h 2>&1)"
body="$("${CEXEC[@]}" "$CNAME" cat "$d" 2>/dev/null || true)"
want "span is kept"                             "OnUnitActiveSec=7h" "$body"
want "paired with a calendar backstop"          "OnCalendar=" "$body"
rt="$("${SC[@]}" show "$UNIT" -p NextElapseUSecRealtime --value 2>/dev/null)"
if [ -n "$rt" ]; then ok "timer is armed even with a monotonic span (backstop fires)"
else bad "timer is armed even with a monotonic span" "no next elapse"; fi

# ---------------------------------------------------------------------------
echo
echo "clear — the override is removed, the timer reverts to the template"
# ---------------------------------------------------------------------------
out="$("${CEXEC[@]}" \
    -e "SPIRA_RUN=${SPIRA_RUN_CTR}" \
    -e "SPIRA_SYSTEMD_USER_DIR=${UDIR}" \
    "$CNAME" "${CADENCE[@]}" clear "$UNIT" 2>&1)"
want "it says it restored the template"         "restored to its template" "$out"
gone="$("${CEXEC[@]}" "$CNAME" bash -c "[ -r '$d' ] && echo exists || echo gone" 2>/dev/null)"
want "the drop-in is gone"                      "gone" "$gone"

# ---------------------------------------------------------------------------
echo
echo "naming — an unknown unit is refused, not guessed"
# ---------------------------------------------------------------------------
out="$("${CEXEC[@]}" \
    -e "SPIRA_RUN=${SPIRA_RUN_CTR}" \
    -e "SPIRA_SYSTEMD_USER_DIR=${UDIR}" \
    "$CNAME" "${CADENCE[@]}" show "definitely-not-a-real-unit" 2>&1)"
want "unknown unit names the fact, not a guess" "no installed timer matches" "$out"

# ---------------------------------------------------------------------------
echo
echo "verify — a timer whose service is running is not reported as dead"
# A monotonic timer reports infinity while its service is active or activating;
# that is not a fault. Misreporting it fires exactly when operators are busiest.
# ---------------------------------------------------------------------------
"${CEXEC[@]}" "$CNAME" bash -c "
cat > '${UDIR}/${SVC}' <<'SVCEOF'
[Unit]
Description=cadence.sh test scratch unit
[Service]
Type=oneshot
ExecStart=/bin/sleep 8
SVCEOF
systemctl --user daemon-reload  # hermetic-ok: inside container via podman exec
systemctl --user start '${SVC}'  # hermetic-ok: inside container via podman exec
" >&2
# Give the service a moment to enter active/activating state.
sleep 2
svc_st="$("${SC[@]}" show "$SVC" -p ActiveState --value 2>/dev/null)"
if [ "$svc_st" = "active" ] || [ "$svc_st" = "activating" ]; then
    out="$("${CEXEC[@]}" \
        -e "SPIRA_RUN=${SPIRA_RUN_CTR}" \
        -e "SPIRA_SYSTEMD_USER_DIR=${UDIR}" \
        "$CNAME" "${CADENCE[@]}" verify "$UNIT" 2>&1)"
    hasnt "mid-run timer is not called dead"    "NEVER FIRES" "$out"
    ok "verify skips timer whose service is active"
else
    printf '  (skipped: could not hold the scratch service in an active state)\n' >&2
fi
# Let the service finish; do not leave an active sleep in the fixture.
"${CEXEC[@]}" "$CNAME" bash -c "systemctl --user stop '${SVC}'" >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
echo
echo "verify — an empty sweep refuses to report all-clear"
# Zero timers in the sweep means the check found nothing to check. Reporting
# 'all armed' on nothing displaces the suspicion that would have prompted a look
# (law-absence-needs-a-positive-control).
# ---------------------------------------------------------------------------
EMPTY_CTR="/tmp/cadence-empty-$$"
"${CEXEC[@]}" "$CNAME" bash -c "mkdir -p '${EMPTY_CTR}'" >/dev/null 2>&1
out="$("${CEXEC[@]}" \
    -e "SPIRA_RUN=${SPIRA_RUN_CTR}" \
    -e "SPIRA_SYSTEMD_USER_DIR=${EMPTY_CTR}" \
    "$CNAME" "${CADENCE[@]}" verify 2>&1)"; rc=$?
"${CEXEC[@]}" "$CNAME" bash -c "rmdir '${EMPTY_CTR}'" >/dev/null 2>&1 || true
want "it says it found nothing to check"        "refusing to report all-clear" "$out"
[ "$rc" -ne 0 ] && ok "and exits non-zero" || bad "and exits non-zero" "exited 0 on empty sweep"

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
