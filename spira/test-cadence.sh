#!/usr/bin/env bash
#
# test-cadence.sh — watch-refresh.sh detects stale watchers against a real systemd container.
#
#   ./test-cadence.sh
#
# WHAT THIS TESTS. watch-refresh.sh queries systemctl to find active watcher units.
# The query uses `watch_unit_name` in conf.sh to build the installed unit name
# (`spira-watch-<name>-<instance>.service`). On unfixed code, it used the template form
# (`spira-watch@<name>.service`), which systemd has no installed instance of — so every
# query came back inactive, and no stale watcher was ever detected or restarted.
#
# This suite runs inside a container with real systemd to prove the fix. The key assertion
# is that `watch-refresh.sh --dry-run` names the installed unit (`spira-watch-testcadence-prod.service`)
# rather than coming back empty or naming the template form.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control):
#   The manifest is created with an old mtime so the watcher starts out NOT stale, then
#   touched to now so it IS stale. The assertion fires only after the touch.
#
# HOST ISOLATION:
#   The host's ~/.config/systemd/user is snapshotted before and after; any change fails the test.
#   All systemd operations run inside the container as spirauser, never on the host.
#
# SKIP CONDITION: no podman on PATH, or user systemd not available in the container.
#
# runtime: ~2m
# covers: spira/watch-refresh.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-cadence.sh"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-cadence.sh: podman not found on PATH\n' >&2
    exit 77
}

TESTENV="$HERE/testenv.sh"
CNAME="spira-testenv-cad-$$"
STUBS_CTR="/tmp/spira-stubs"
WATCHERS_CTR="/tmp/test-watchers"
SPIRA_RUN_CTR="/tmp/spira-cad"

cleanup() {
    bash "$TESTENV" down --name "$CNAME" --volumes >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

bash "$TESTENV" up --name "$CNAME" >&2
iszero "up exits 0" "$?"

if ! bash "$TESTENV" probe --name "$CNAME"; then
    printf 'SKIP test-cadence.sh: user systemd not available in container\n' >&2
    exit 77
fi
ok "user systemd running (probe exits 0)"

# HOST SNAPSHOT — must be captured before any container work. A diff at the end proves
# the container test never leaks unit files to the host's own systemd directory.
snap_host_before="$(ls -1 "$HOME/.config/systemd/user/" 2>/dev/null | sort || true)"

# ---------------------------------------------------------------------------
# BASE EXECUTION ARRAY. Same pattern as test-install-rehearsal.sh.
# SPIRA_RUN is redirected to an ephemeral path inside the container so the
# runtime tree (watchd/ logs, cursors, world.halted) never touches the host.
# ---------------------------------------------------------------------------
CEXEC=(podman exec --user spirauser
    -e XDG_RUNTIME_DIR=/run/user/1001
    -e "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1001/bus"
    -e "SPIRA_PATH=${STUBS_CTR}"
    -e "SPIRA_BD=${STUBS_CTR}/bd"
    -e "SPIRA_RUN=${SPIRA_RUN_CTR}"
    -e "SPIRA_WORKSPACES=/tmp"
    -e "SPIRA_HOME_REPO=home"
)

# ---------------------------------------------------------------------------
# CREATE STUBS AND FAKE PROD DIRECTORY inside the container as spirauser.
# Same stubs as test-install-rehearsal.sh; the fake prod dir makes doctor.sh
# see split-checkout mode and install.sh find executable ExecStart targets.
# ---------------------------------------------------------------------------
"${CEXEC[@]}" "$CNAME" bash -c "
mkdir -p '${STUBS_CTR}'

cat > '${STUBS_CTR}/bd' << 'STUBEOF'
#!/bin/sh
while [ \$# -gt 0 ]; do
    case \"\$1\" in
        -C) shift; [ \$# -gt 0 ] && shift ;;
        list|memories|recall|children) printf '[]\n'; exit 0 ;;
        *) shift ;;
    esac
done
exit 0
STUBEOF
chmod +x '${STUBS_CTR}/bd'

printf '#!/bin/sh\nexit 0\n' > '${STUBS_CTR}/loom'
chmod +x '${STUBS_CTR}/loom'

cat > '${STUBS_CTR}/loom-probe' << 'LPEOF'
#!/bin/sh
printf '200 5ms\n'
LPEOF
chmod +x '${STUBS_CTR}/loom-probe'

mkdir -p /tmp/spira-prod && cp -a /workspace/spira /tmp/spira-prod/
" >&2
iszero "stubs created inside container" "$?"

# ---------------------------------------------------------------------------
# WATCHERS MANIFEST — a single daemon row pointing at /bin/sleep so the unit
# stays active without needing a database. Created with an old mtime so the
# watcher starts life NOT stale; we touch it later to trigger the staleness.
# ---------------------------------------------------------------------------
"${CEXEC[@]}" "$CNAME" bash -c "
printf 'testcadence|daemon|/bin/sleep 3600|\n' > '${WATCHERS_CTR}'
touch -d '@1735689600' '${WATCHERS_CTR}'
" >&2
iszero "test watchers manifest created" "$?"

# ===========================================================================
echo
echo "configure:"
# ===========================================================================
"${CEXEC[@]}" \
    -e "CONFIGURE_PROD=/tmp/spira-prod/spira" \
    -e "CONFIGURE_MAX_AEONS=1" \
    -e "CONFIGURE_MAX_LIVE_AEONS=1" \
    -e "CONFIGURE_LOOM_ADDR=127.0.0.1:7300" \
    -e "CONFIGURE_DOLT_DATA=" \
    "$CNAME" bash /workspace/spira/configure.sh >&2
iszero "configure.sh exits 0" "$?"

"${CEXEC[@]}" "$CNAME" bash -c \
    'mkdir -p "$HOME/.local/share/spira/db/.beads"' >&2
iszero "fake database .beads created" "$?"

# ===========================================================================
echo
echo "install — SPIRA_WATCHERS overrides to the single-row test manifest:"
# ===========================================================================
"${CEXEC[@]}" "$CNAME" bash -c \
    "mkdir -p '${SPIRA_RUN_CTR}' && touch '${SPIRA_RUN_CTR}/world.halted'" >&2
iszero "world.halted created" "$?"

"${CEXEC[@]}" \
    -e "SPIRA_INSTALL_FORCE=1" \
    -e "SPIRA_WATCHERS=${WATCHERS_CTR}" \
    "$CNAME" bash /workspace/systemd/install.sh >&2
iszero "install.sh exits 0" "$?"

# THE UNIT RULE. install.sh always appends -$SPIRA_INSTANCE, prod included.
# The PATH-suffix rule (conf.sh's _spira_inst_sfx) leaves prod with no suffix;
# if watch-refresh.sh used that rule the unit name would not match.
unit_exists="$("${CEXEC[@]}" "$CNAME" bash -c \
    '[ -f "$HOME/.config/systemd/user/spira-watch-testcadence-prod.service" ] && echo yes || echo no')"
want "spira-watch-testcadence-prod.service installed (UNIT rule)" "yes" "$unit_exists"

# ===========================================================================
echo
echo "start watcher — make it active so staleness can be detected:"
# ===========================================================================
"${CEXEC[@]}" "$CNAME" bash -c \
    "rm -f '${SPIRA_RUN_CTR}/world.halted'" >&2

"${CEXEC[@]}" \
    -e "SPIRA_WATCHERS=${WATCHERS_CTR}" \
    "$CNAME" bash -c \
    'systemctl --user start spira-watch-testcadence-prod.service' >&2
iszero "start spira-watch-testcadence-prod.service exits 0" "$?"

active_out="$("${CEXEC[@]}" "$CNAME" bash -c \
    'systemctl --user is-active spira-watch-testcadence-prod.service 2>&1')"
want "spira-watch-testcadence-prod.service is active" "active" "$active_out"

# ===========================================================================
echo
echo "staleness detection — the unit-name resolution property:"
# ===========================================================================
# Touch the manifest to a time AFTER the unit started. The manifest is one of
# the files watch-refresh.sh tracks as the "code" of every watcher. A manifest
# newer than the unit's start time means the watcher is stale.
#
# On unfixed code: watch-refresh.sh queries spira-watch@testcadence.service
# (template form). systemd has no instance of that template, so it returns
# ActiveState=inactive. No active unit is found; nothing is reported. The
# assertion below fails: the expected unit name is absent from the output.
#
# On fixed code: watch-refresh.sh queries spira-watch-testcadence-prod.service
# (installed form). systemd reports it as active. The mtime check finds the
# manifest newer than the unit's start time. --dry-run reports the restart.
"${CEXEC[@]}" \
    -e "SPIRA_WATCHERS=${WATCHERS_CTR}" \
    "$CNAME" bash -c "touch '${WATCHERS_CTR}'" >&2
iszero "manifest touched to simulate staleness" "$?"

dry_out="$("${CEXEC[@]}" \
    -e "SPIRA_WATCHERS=${WATCHERS_CTR}" \
    "$CNAME" bash /workspace/spira/watch-refresh.sh --dry-run 2>&1)"
want "watch-refresh detects stale watcher by installed unit name" \
    "would restart spira-watch-testcadence-prod.service" "$dry_out"
notwant "not by template form (which systemd has no installed instance of)" \
    "spira-watch@testcadence.service" "$dry_out"

# ===========================================================================
echo
echo "host isolation — container test writes nothing to the host's unit directory:"
# ===========================================================================
snap_host_after="$(ls -1 "$HOME/.config/systemd/user/" 2>/dev/null | sort || true)"
snap_diff="$(diff <(printf '%s\n' "$snap_host_before") <(printf '%s\n' "$snap_host_after") || true)"
[ -z "$snap_diff" ] \
    && ok "host ~/.config/systemd/user is unchanged (diff empty)" \
    || bad "host unit directory changed after container test" \
           "$(printf '%s\n' "$snap_diff" | head -10)"

echo
printf '%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
