#!/usr/bin/env bash
# lib-test-install-rehearsal.sh — the container both install-rehearsal suites run in: up, stubbed
# bd/loom, the fake release root, the tree's built binaries streamed in, configure.sh run.
# Sourced by test-install-rehearsal.sh and test-install-rehearsal-stray.sh after they source
# testlib.sh and podman-exec-diag.sh and define iszero. Never executed. What is real and what
# is stubbed is stated in test-install-rehearsal.sh's header.

command -v podman >/dev/null 2>&1 || skip "podman not on PATH"
CNAME="spira-testenv-reh-$$"
# Stubs live inside the container at /tmp/spira-stubs, created as spirauser.
# A bind-mount cannot carry execute permissions reliably across rootless podman's
# UID namespace, so creating stubs inside the container is the only approach that
# works without platform-specific flags.
STUBS_CTR="/tmp/spira-stubs"

cleanup() {
    testenv container down --name "$CNAME" --volumes >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

testenv container up --name "$CNAME" >&2
iszero "up exits 0" "$?"

if ! testenv container probe --name "$CNAME"; then
    printf 'SKIP test-install-rehearsal.sh: user systemd not available in container\n' >&2
    exit 77
fi
ok "user systemd running (probe exits 0)"

# ---------------------------------------------------------------------------
# BASE EXECUTION ARRAY. Builds the common prefix for podman exec as spirauser.
# Extra -e flags and "$CNAME" CMD are appended by each call site.
#
# THIS SUITE IS THE LAUNCHER of the container's processes, so it sets their PATH
# (sp-gypjk): the stubs first, then the fake prod root's release layout (bin/, spira/) —
# every tool is invoked by name. Every script that sources conf.sh (install.sh,
# uninstall.sh, ready.sh, sentinel.sh, seed.sh) then finds stub bd and loom on PATH. conf.sh also exports SPIRA_BD
# if it is not already set, but we set it explicitly here so the schema check
# (conf.sh calls "$SPIRA_BD" -C "$SPIRA_DB" migrate schema) uses the stub
# before the PATH rebuild has happened.
#
# SPIRA_RUN is overridden to /tmp/spira-reh (inside the container) so the runtime
# tree — world.halted, watchd/, the ledger — is ephemeral: it lives only inside the
# container and disappears when the container is removed. The host filesystem is
# not polluted.
# ---------------------------------------------------------------------------
SPIRA_RUN_CTR="/tmp/spira-reh"
# The timeout is a hang guard, never a budget (law-no-wall-clock-budgets-in-the-corpus): a 5 s cap
# killed a successful exec on a loaded 4-vCPU publish runner (PR 537, case 30, exit 124).
CEXEC=(timeout 120 podman exec --user spirauser
    -e XDG_RUNTIME_DIR=/run/user/1001
    -e "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1001/bus"
    -e "PATH=${STUBS_CTR}:/tmp/spira-prod/bin:/tmp/spira-prod/spira:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
    -e "SPIRA_PATH=${STUBS_CTR}"
    -e "SPIRA_BD=${STUBS_CTR}/bd"
    -e "SPIRA_RUN=${SPIRA_RUN_CTR}"
    -e "SPIRA_WORKSPACES=/tmp"
    -e "SPIRA_HOME_REPO=home"
    -e "SPIRA_ADOPT_CONFIGURE_DEFAULT=1"
    # This container has nobody at the console reading escalations; doctor's
    # operator-channel check (hunk, mail client) is documented to FAIL only on an
    # operated box and WARN on "a headless fixture or CI box" — which this is.
    -e "SPIRA_OPERATED=0"
)
# Install runs a dependency phase and doctor: past the 5s a single probe is given.
_reh_first_run="$(sed -n 's/^const FIRST_RUN_SECS: &str = "\([0-9]*\)";.*/\1/p' "$HERE/../install/src/bin/install.rs")"
_reh_cockpit_wait="$(sed -n 's/^_rdy_first_wait_s=.*:-\([0-9]*\)}.*/\1/p' "$HERE/ready.sh")"
[ -n "$_reh_first_run" ] && [ -n "$_reh_cockpit_wait" ] || { echo "FAIL cannot derive install guard from install.rs/ready.sh"; exit 1; }
INSTALL_GUARD_S=$(( _reh_first_run + _reh_cockpit_wait + 60 ))
INSTALL_EXEC=(timeout "$INSTALL_GUARD_S" "${CEXEC[@]:2}")

# ---------------------------------------------------------------------------
# CREATE STUB SCRIPTS INSIDE THE CONTAINER as spirauser so they are owned by
# UID 1001 and have no UID-namespace execute-permission issues.
#
# bd stub: handles `bd -C <path> <subcommand> [args]` as called by bdq in lib.sh.
# Exits 0 for all calls; prints [] for subcommands that return JSON arrays.
# The `migrate schema` call from conf.sh's schema check also exits 0 (no output),
# which the check interprets as "no mismatch".
#
# loom stub: found by name on PATH (STUBS_CTR first).
# loom-probe stub: must print "200 Nms" to pass the loom check in ready.sh.
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

# Create a fake prod checkout OUTSIDE /workspace for SPIRA_PROD (CONFIGURE_PROD) to
# name — a split-checkout SPIRA_PROD is what configure.sh writes by default. install.sh
# checks that ExecStart targets are executable, so the scripts must be present. Unit
# templates resolve three placeholders against this one checkout root (render.py):
# @SPIRA_PROD@ (the spira/ subdir), @SPIRA_PROD_COCK@ (its cockpit/ sibling) and
# @SPIRA_PROD_ROOT@ (the root itself, for concierge.sh) — all three
# need real files here or install.sh refuses the first unit that names one.
mkdir -p /tmp/spira-reh-rel/r1 /tmp/spira-reh-home
ln -sfn r1 /tmp/spira-reh-rel/current
ln -sfn /tmp/spira-reh-rel/r1 /tmp/spira-prod
cp -a /workspace/spira /tmp/spira-prod/
cp -a /workspace/cockpit /tmp/spira-prod/
cp -a /workspace/concierge.sh /tmp/spira-prod/
# The release's bin/: the units ExecStart /tmp/spira-prod/bin/<tool> (sp-gypjk). world.halted
# (below) means units are only enabled, never started, so no-op stubs satisfy install.sh's
# executable-bit check. NOT PROVEN here: the tree's own build staged as this bin/ (sp-isom7).
mkdir -p /tmp/spira-prod/bin
for b in sentinel queue dwarf spira-supervise landing-pass reconciler-flow hel; do
    printf '#!/bin/sh\nexit 0\n' > /tmp/spira-prod/bin/\$b && chmod +x /tmp/spira-prod/bin/\$b
done
" >&2
iszero "stubs created inside container" "$?"

# The tree's whole build, staged as the fake release's bin/ beside the no-op ExecStart
# stubs: install's preflight runs doctor, which requires every release tool to resolve on
# PATH, the gate and the watcher manifest to answer. The stubbed names stay stubs.
_reh_bin="$(dirname "$(command -v spira-install 2>/dev/null)")"
[ -d "$_reh_bin" ] || bail "spira-install is not on PATH (the tree's build provides it)"
tar -C "$_reh_bin" --exclude=sentinel --exclude=queue --exclude=dwarf --exclude=spira-supervise --exclude=landing-pass --exclude=reconciler-flow --exclude=hel -cf - . > /tmp/spira-reh-bin.tar
# batch-job: streaming the tree's release bin/ into the rehearsal container
podman exec -i --user spirauser "$CNAME" tar -C /tmp/spira-prod/bin -xf - < /tmp/spira-reh-bin.tar >&2
iszero "the tree's release bin/ is staged in the container" "$?"

# The release bin/ above carries the tree's own spira-config (the image has none: testenv/
# Containerfile removes it, and conf.sh fails closed without it). Compare bytes, not presence.
_reh_staged() {
    local want got
    want="$(sha256sum "$2" | cut -d' ' -f1)"
    got="$("${CEXEC[@]}" "$CNAME" sh -c "test -x '$1' && sha256sum '$1'" 2>/dev/null | cut -d' ' -f1)"
    [ -n "$want" ] && [ "$want" = "$got" ]
}
_sc="$(command -v spira-config 2>/dev/null || true)"
[ -n "$_sc" ] || bail "spira-config is not on PATH (the tree's build provides it)"
_reh_staged /tmp/spira-prod/bin/spira-config "$_sc"
iszero "the tree's spira-config is staged in the container's release bin/" "$?"

# ===========================================================================
echo
echo "configure — non-interactive spira.conf bootstrap:"
# ===========================================================================
# CONFIGURE_PROD=/tmp/spira-reh-rel/current/spira is a fake prod path OUTSIDE SPIRA_REPO
# (/workspace) — a split-checkout SPIRA_PROD. The directory was created in the
# stubs block above so it already exists when configure.sh writes it out.
# CONFIGURE_DOLT_DATA="" suppresses the dolt-beads.service unit (no Dolt here).
"${CEXEC[@]}" \
    -e "CONFIGURE_PROD=/tmp/spira-reh-rel/current/spira" \
    -e "CONFIGURE_HOME_REPO=home" \
    -e "CONFIGURE_HOME_REPO_PATH=/tmp/spira-reh-home" \
    -e "CONFIGURE_RELEASES=/tmp/spira-reh-rel" \
    -e "CONFIGURE_RUN=${SPIRA_RUN_CTR}" \
    -e "CONFIGURE_OPERATED=0" \
    -e "CONFIGURE_MAX_DWARVES=1" \
    -e "CONFIGURE_MAX_LIVE_DWARVES=1" \
    -e "CONFIGURE_LOOM_ADDR=127.0.0.1:7300" \
    -e "CONFIGURE_DOLT_DATA=" \
    "$CNAME" bash /workspace/spira/configure.sh >&2
iszero "configure.sh exits 0" "$?"

# configure.sh and ready.sh run before install puts the release's bin/ on PATH: they must
# find spira-config in their own release tree. PATH here holds no Spira directory at all.
"${CEXEC[@]}" \
    -e "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    -e "CONFIGURE_OUT=/tmp/spira-reh-nopath.conf" \
    -e "CONFIGURE_PROD=/tmp/spira-reh-rel/current/spira" \
    -e "CONFIGURE_RELEASES=/tmp/spira-reh-rel" \
    -e "CONFIGURE_RUN=${SPIRA_RUN_CTR}" \
    -e "CONFIGURE_MAX_DWARVES=1" \
    -e "CONFIGURE_MAX_LIVE_DWARVES=1" \
    -e "CONFIGURE_LOOM_ADDR=127.0.0.1:7300" \
    -e "CONFIGURE_DOLT_DATA=" \
    "$CNAME" bash /tmp/spira-prod/spira/configure.sh >&2
iszero "configure.sh exits 0 with a PATH lacking the release bin" "$?"

# ===========================================================================
echo
echo "stage spira-install (sp-31dm0: systemd/install.sh is retired; it is a compiled binary now):"
# ===========================================================================
# The tree's own build of spira-install is in the release bin/ streamed above; the rehearsal
# installs the built binary and never compiles (per Ryan 2026-10-05).
_si="$(command -v spira-install 2>/dev/null || true)"
[ -n "$_si" ] || bail "spira-install is not on PATH (the tree's build provides it)"
INSTALL_BIN="/tmp/spira-install"
"${CEXEC[@]}" "$CNAME" cp /tmp/spira-prod/bin/spira-install "$INSTALL_BIN" >&2 \
    && _reh_staged "$INSTALL_BIN" "$_si"
iszero "the tree's spira-install is staged in the container" "$?"

_doc="$(command -v doctor 2>/dev/null || true)"
[ -n "$_doc" ] || bail "doctor is not on PATH (the tree's build provides it)"
_reh_staged /tmp/spira-prod/bin/spira-install "$_si" && _reh_staged /tmp/spira-prod/bin/doctor "$_doc"
iszero "the tree's spira-install and doctor are staged in the container's release bin/" "$?"

_reh_staged /tmp/spira-prod/bin/spira-config "$_sc"
iszero "the tree's spira-config is staged in the container's release bin/" "$?"

# Create the fake database marker. The .beads directory satisfies directory-existence
# checks in ready.sh ("database absent — no .beads") and seed.sh without requiring
# a real Dolt store or any bd migration state. The stub bd handles all list/memories
# calls and exits 0 for the conf.sh schema check.
"${CEXEC[@]}" "$CNAME" bash -c \
    'mkdir -p "$HOME/.local/share/spira/db/.beads"' >&2
iszero "fake database .beads created" "$?"

