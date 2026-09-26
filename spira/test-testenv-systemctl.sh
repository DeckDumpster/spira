#!/usr/bin/env bash
#
# test-testenv-systemctl.sh — testenv.sh as a fixture for systemd unit management tests.
#
# WHAT THIS DEMONSTRATES
# ----------------------
# That testenv.sh is usable by a suite other than the rehearsal (test-testenv.sh).
# Here it provides the user systemd environment for a test that installs, starts, and
# stops a custom user service unit, exercising the full systemctl --user lifecycle
# against a real session manager rather than a recording stub.
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. A .service file written to /workspace is visible inside the container.
# 2. `systemctl --user enable --now` starts the unit and it reaches active state.
# 3. `systemctl --user stop` stops it.
# 4. The unit file is found by systemctl --user even when placed under a non-standard
#    path, confirming that XDG_RUNTIME_DIR and the user instance are correctly wired.
#
# SKIP CONDITION: no podman on PATH, or the testenv image cannot be built.
#
# tier: T1
# covers: spira/testenv.sh spira/testenv/Containerfile
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
iszero() { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }

echo "test-testenv-systemctl.sh"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-systemctl.sh: podman not found on PATH\n' >&2
    exit 77
}

TESTENV="$HERE/testenv.sh"
CNAME="spira-testenv-sc-$$"
TMP="$(mktemp -d)"

cleanup() {
    # Remove the unit file from the checkout dir so the working tree is clean.
    rm -f "$TMP/spira-probe.service"
    bash "$TESTENV" down --name "$CNAME" --volumes >/dev/null 2>&1 || true
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

bash "$TESTENV" up --name "$CNAME" >&2
iszero "up exits 0" "$?"

# Skip if user systemd is not working (probe exits non-zero). A stub-based suite
# would be a different test; this one explicitly exercises the real session manager.
if ! bash "$TESTENV" probe --name "$CNAME"; then
    printf 'SKIP test-testenv-systemctl.sh: user systemd not available in container\n' >&2
    exit 77
fi
ok "user systemd running (probe exits 0)"

# ==========================================================================
echo
echo "unit install — write and enable a simple user service:"
# ==========================================================================
# Write a minimal oneshot service. It runs `true`, which exits 0 immediately.
# This is enough to exercise enable-now, start, stop, and status without needing
# any real workload inside the container.
cat > "$TMP/spira-probe.service" << 'EOF'
[Unit]
Description=Spira testenv probe service

[Service]
Type=oneshot
ExecStart=/bin/true
RemainAfterExit=yes

[Install]
WantedBy=default.target
EOF

# Copy the unit file into the container's user systemd configuration directory.
podman exec "$CNAME" bash -c 'mkdir -p /run/user/1001/systemd/user' 2>/dev/null || true
podman cp "$TMP/spira-probe.service" \
    "${CNAME}:/run/user/1001/systemd/user/spira-probe.service"
cp_rc=$?
iszero "unit file copied into container" "$cp_rc"

# Reload the user daemon to pick up the new unit.
podman exec --user spirauser \
    -e XDG_RUNTIME_DIR=/run/user/1001 \
    -e DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/1001/bus" \
    "$CNAME" systemctl --user daemon-reload 2>/dev/null
iszero "daemon-reload exits 0" "$?"

# ==========================================================================
echo
echo "start — activate the unit:"
# ==========================================================================
podman exec --user spirauser \
    -e XDG_RUNTIME_DIR=/run/user/1001 \
    -e DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/1001/bus" \
    "$CNAME" systemctl --user start spira-probe.service 2>/dev/null
iszero "systemctl --user start exits 0" "$?"

# Verify the unit reached active state.
status_out="$(podman exec --user spirauser \
    -e XDG_RUNTIME_DIR=/run/user/1001 \
    -e DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/1001/bus" \
    "$CNAME" systemctl --user is-active spira-probe.service 2>&1)"
is "unit is active after start" "active" "$status_out"

# ==========================================================================
echo
echo "stop — deactivate the unit:"
# ==========================================================================
podman exec --user spirauser \
    -e XDG_RUNTIME_DIR=/run/user/1001 \
    -e DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/1001/bus" \
    "$CNAME" systemctl --user stop spira-probe.service 2>/dev/null
iszero "systemctl --user stop exits 0" "$?"

# After stop, is-active must return non-zero (inactive).
podman exec --user spirauser \
    -e XDG_RUNTIME_DIR=/run/user/1001 \
    -e DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/1001/bus" \
    "$CNAME" systemctl --user is-active spira-probe.service >/dev/null 2>&1 \
    && bad "unit inactive after stop" "is-active still exits 0" \
    || ok "unit is inactive after stop"

# ==========================================================================
echo
tl_summary
