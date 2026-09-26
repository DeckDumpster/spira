#!/usr/bin/env bash
# test-install-rehearsal.sh — end-to-end install → assert → uninstall → clean-state cycle
#
# REAL SYSTEMD vs STUBS:
#   Real systemd  — unit files installed to ~/.config/systemd/user/ (real systemctl --user),
#                   sentinel timer enabled and started (real user session manager)
#   Stub          — bd (container has no bd; stub created as spirauser inside the container
#                   at /tmp/spira-stubs/bd satisfies conf.sh schema check, doctor.sh
#                   fatal-binary check, and ready.sh database query)
#   Stub          — SPIRA_LOOM_BIN (fake executable satisfies the binary-present gate in
#                   ready.sh; SPIRA_LOOM_PROBE then handles the actual probe call)
#   Stub          — SPIRA_LOOM_PROBE (script prints "200 5ms" in place of HTTP call)
#   Stub          — SPIRA_SUPERVISE_BIN, only when the checkout has no cargo build
#                   (gate.yml's own run has a real one in bin/); a bare exit-0 script
#                   satisfies install.sh's executable-target refusal for spira-cockpit.service
#                   without paying a Rust build inside the container
#   Env override  — SPIRA_OPERATED=0 (this rehearsal has nobody at the console; doctor.sh's
#                   operator-channel check is documented to WARN rather than FAIL here)
#   Warn path     — tmux (absent in container; layout.sh pane check WARNs, not FAILs)
#   NOT PROVEN    — filed bead reaches ready + sentinel.sh --report names it
#                   (operational bd+database required for bead filing; skipped here)
#   NOT PROVEN    — cockpit collector writes a snapshot (spira-cockpit-prod.service is
#                   enabled, like most units here, but never started; ready.sh's cockpit
#                   check FAILs on that alone, so it is asserted around rather than as
#                   part of the overall exit code)
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control):
#   The stray sweep section re-installs, plants a unit not in the owned manifest, then runs
#   uninstall and verifies that "STRAY" appears in the output. Only then does a clean run
#   (with no stray planted) count as evidence that the sweep is clean.
#
# SKIP CONDITION: no podman on PATH, or user systemd not available in the container.
#
# runtime: ~3m
# covers: systemd/install.sh spira/uninstall.sh spira/configure.sh spira/testenv.sh spira/testenv/Containerfile
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

. "$HERE/testlib.sh"
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }

echo "test-install-rehearsal.sh"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-install-rehearsal.sh: podman not found on PATH\n' >&2
    exit 77
}

TESTENV="$HERE/testenv.sh"
CNAME="spira-testenv-reh-$$"
# Stubs live inside the container at /tmp/spira-stubs, created as spirauser.
# A bind-mount cannot carry execute permissions reliably across rootless podman's
# UID namespace, so creating stubs inside the container is the only approach that
# works without platform-specific flags.
STUBS_CTR="/tmp/spira-stubs"

cleanup() {
    bash "$TESTENV" down --name "$CNAME" --volumes >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

bash "$TESTENV" up --name "$CNAME" >&2
iszero "up exits 0" "$?"

if ! bash "$TESTENV" probe --name "$CNAME"; then
    printf 'SKIP test-install-rehearsal.sh: user systemd not available in container\n' >&2
    exit 77
fi
ok "user systemd running (probe exits 0)"

# ---------------------------------------------------------------------------
# BASE EXECUTION ARRAY. Builds the common prefix for podman exec as spirauser.
# Extra -e flags and "$CNAME" CMD are appended by each call site.
#
# SPIRA_PATH is prepended by conf.sh when it rebuilds PATH. Every script that
# sources conf.sh (install.sh, uninstall.sh, ready.sh, sentinel.sh, seed.sh)
# then finds stub bd on PATH via `command -v bd`. conf.sh also exports SPIRA_BD
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
CEXEC=(podman exec --user spirauser
    -e XDG_RUNTIME_DIR=/run/user/1001
    -e "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1001/bus"
    -e "SPIRA_PATH=${STUBS_CTR}"
    -e "SPIRA_BD=${STUBS_CTR}/bd"
    -e "SPIRA_RUN=${SPIRA_RUN_CTR}"
    -e "SPIRA_WORKSPACES=/tmp"
    -e "SPIRA_HOME_REPO=home"
    # This container has nobody at the console reading escalations; doctor.sh's
    # operator-channel check (hunk, mail client) is documented to FAIL only on an
    # operated box and WARN on "a headless fixture or CI box" — which this is.
    -e "SPIRA_OPERATED=0"
)

# ---------------------------------------------------------------------------
# CREATE STUB SCRIPTS INSIDE THE CONTAINER as spirauser so they are owned by
# UID 1001 and have no UID-namespace execute-permission issues.
#
# bd stub: handles `bd -C <path> <subcommand> [args]` as called by bdq in lib.sh.
# Exits 0 for all calls; prints [] for subcommands that return JSON arrays.
# The `migrate schema` call from conf.sh's schema check also exits 0 (no output),
# which the check interprets as "no mismatch".
#
# loom stub: only needs to be executable so SPIRA_LOOM_BIN passes the -x gate.
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
# @SPIRA_PROD_ROOT@ (the root itself, for beads-push.sh and concierge.sh) — all three
# need real files here or install.sh refuses the first unit that names one.
mkdir -p /tmp/spira-prod
cp -a /workspace/spira /tmp/spira-prod/
cp -a /workspace/cockpit /tmp/spira-prod/
cp -a /workspace/beads-push.sh /workspace/concierge.sh /tmp/spira-prod/
" >&2
iszero "stubs created inside container" "$?"

# ---------------------------------------------------------------------------
# spira-cockpit.service's ExecStart is @SPIRA_SUPERVISE_BIN@ (a compiled Rust binary,
# unlike the shell-script units above), and unlike loom/broker/landing-pass it is not
# gated on that binary's executability — install.sh refuses an unexecutable ExecStart
# target outright, so a checkout with no cargo build cannot install at all.
#
# The gate's build job leaves a real binary in /workspace/bin, which spirauser can
# read (the bind mount preserves the host's chmod +x) but not write to — /workspace
# is bind-mounted with the host's ownership. So a missing binary is stubbed under
# STUBS_CTR instead (spirauser-writable) and named to install.sh via an env override,
# rather than by trying to create the file in place. world.halted (below) means the
# unit is only enabled, never started, so a stub that merely satisfies the
# executable-bit check is sufficient.
# ---------------------------------------------------------------------------
SUPERVISE_ENV=()
if "${CEXEC[@]}" "$CNAME" bash -c \
    '[ -x /workspace/bin/spira-supervise ] || [ -x /workspace/target/release/spira-supervise ]' \
    >/dev/null 2>&1; then
    ok "spira-supervise: real binary found in the checkout"
else
    "${CEXEC[@]}" "$CNAME" bash -c \
        "printf '#!/bin/sh\nexit 0\n' > '${STUBS_CTR}/spira-supervise' && chmod +x '${STUBS_CTR}/spira-supervise'" >&2
    iszero "spira-supervise stub created (no cargo build in this checkout)" "$?"
    SUPERVISE_ENV=(-e "SPIRA_SUPERVISE_BIN=${STUBS_CTR}/spira-supervise")
fi

# ===========================================================================
echo
echo "configure — non-interactive spira.conf bootstrap:"
# ===========================================================================
# CONFIGURE_PROD=/tmp/spira-prod/spira is a fake prod path OUTSIDE SPIRA_REPO
# (/workspace) — a split-checkout SPIRA_PROD. The directory was created in the
# stubs block above so it already exists when configure.sh writes it out.
# CONFIGURE_DOLT_DATA="" suppresses the dolt-beads.service unit (no Dolt here).
"${CEXEC[@]}" \
    -e "CONFIGURE_PROD=/tmp/spira-prod/spira" \
    -e "CONFIGURE_MAX_AEONS=1" \
    -e "CONFIGURE_MAX_LIVE_AEONS=1" \
    -e "CONFIGURE_LOOM_ADDR=127.0.0.1:7300" \
    -e "CONFIGURE_DOLT_DATA=" \
    "$CNAME" bash /workspace/spira/configure.sh >&2
iszero "configure.sh exits 0" "$?"

# Create the fake database marker. The .beads directory satisfies directory-existence
# checks in ready.sh ("database absent — no .beads") and seed.sh without requiring
# a real Dolt store or any bd migration state. The stub bd handles all list/memories
# calls and exits 0 for the conf.sh schema check.
"${CEXEC[@]}" "$CNAME" bash -c \
    'mkdir -p "$HOME/.local/share/spira/db/.beads"' >&2
iszero "fake database .beads created" "$?"

# ===========================================================================
echo
echo "before-install snapshot — record baseline unit-directory state:"
# ===========================================================================
snap_before="$("${CEXEC[@]}" "$CNAME" bash -c \
    'ls "$HOME/.config/systemd/user/" 2>/dev/null | sort || true')"

# ===========================================================================
echo
echo "install — world.halted pre-created; units are enabled but not started:"
# ===========================================================================
# world.halted in SPIRA_RUN causes install.sh to enable units without starting
# them and to skip the end-state check (which would fail: bd/dolt are absent).
# SPIRA_INSTALL_FORCE=1 bypasses the landref check (we are on a worktree branch,
# not the base branch) and the live-aeons guard.
"${CEXEC[@]}" "$CNAME" bash -c \
    "mkdir -p '${SPIRA_RUN_CTR}' && touch '${SPIRA_RUN_CTR}/world.halted'" >&2
iszero "world.halted created" "$?"

"${CEXEC[@]}" \
    -e "SPIRA_INSTALL_FORCE=1" \
    "${SUPERVISE_ENV[@]}" \
    "$CNAME" bash /workspace/systemd/install.sh >&2
iszero "install.sh exits 0" "$?"

# ===========================================================================
echo
echo "post-install assertions — five Intent conditions:"
# ===========================================================================
# 1. Unit files present in UNITDIR (real systemd; install.sh wrote them).
unit_count="$("${CEXEC[@]}" "$CNAME" bash -c \
    'ls "$HOME"/.config/systemd/user/spira-*-prod.* 2>/dev/null | wc -l || echo 0')"
[ "${unit_count:-0}" -gt 0 ] \
    && ok "unit files installed (${unit_count} spira-*-prod.* found)" \
    || bad "unit files installed" "no spira-*-prod.* in ~/.config/systemd/user/"

# 2. Sentinel timer enabled (real systemd; world was halted so enabled not started).
enabled_out="$("${CEXEC[@]}" "$CNAME" \
    bash -c 'systemctl --user is-enabled spira-sentinel-prod.timer 2>&1')"
want "sentinel timer enabled" "enabled" "$enabled_out"

# Remove world.halted and start the sentinel timer so ready.sh sees it as active.
"${CEXEC[@]}" "$CNAME" bash -c \
    "rm -f '${SPIRA_RUN_CTR}/world.halted'" >&2
iszero "world.halted removed" "$?"

"${CEXEC[@]}" "$CNAME" bash -c \
    'systemctl --user start spira-sentinel-prod.timer' >&2
iszero "sentinel timer started" "$?"

active_out="$("${CEXEC[@]}" "$CNAME" \
    bash -c 'systemctl --user is-active spira-sentinel-prod.timer 2>&1')"
want "sentinel timer active" "active" "$active_out"

# 3. World not halted — stamp file is gone.
"${CEXEC[@]}" "$CNAME" bash -c \
    "[ ! -f '${SPIRA_RUN_CTR}/world.halted' ]" \
    && ok "world not halted (stamp absent)" \
    || bad "world not halted" "world.halted still present after removal"

# 4. doctor.sh exits 0 — the stub bd/store fixture created above satisfies its
#    runtime-health checks (store reachable, no failed units, snapshot warns but
#    does not FAIL since the collector has not run yet).
doc_out="$("${CEXEC[@]}" "$CNAME" bash /workspace/spira/doctor.sh 2>&1)"
doc_rc=$?
iszero "doctor.sh exits 0" "$doc_rc"
nowant "doctor.sh: no FAIL at all" "FAIL" "$doc_out"

# 5. ready.sh with stubs. SPIRA_LOOM_BIN points to the stub loom binary so the
#    binary-present gate passes; SPIRA_LOOM_PROBE then returns "200 5ms".
#    sentinel.sh --report renders WARN (no open beads) and seed.sh --list renders
#    WARN (no statutes in the stub db) — both are WARNs, not FAILs. The cockpit
#    collector check is a known FAIL (see NOT PROVEN, header) since nothing here
#    starts spira-cockpit-prod.service, so specific lines are asserted instead of
#    the overall exit code.
ready_out="$("${CEXEC[@]}" \
    -e "SPIRA_LOOM_BIN=${STUBS_CTR}/loom" \
    -e "SPIRA_LOOM_PROBE=${STUBS_CTR}/loom-probe" \
    "$CNAME" bash /workspace/spira/ready.sh 2>&1)"
want "ready.sh: sentinel timer active" "pass  sentinel timer active" "$ready_out"
want "ready.sh: world not halted"     "pass  world not halted"       "$ready_out"
want "ready.sh: database readable"    "pass  database readable"      "$ready_out"
want "ready.sh: loom probe passes"    "loom answers 200"              "$ready_out"

# ===========================================================================
echo
echo "uninstall — remove the installation:"
# ===========================================================================
uninstall_out="$("${CEXEC[@]}" "$CNAME" \
    bash /workspace/spira/uninstall.sh --yes 2>&1)"
iszero "uninstall.sh --yes exits 0" "$?"

# ===========================================================================
echo
echo "post-uninstall snapshot — assert clean state matches pre-install:"
# ===========================================================================
snap_after="$("${CEXEC[@]}" "$CNAME" bash -c \
    'ls "$HOME/.config/systemd/user/" 2>/dev/null | sort || true')"

snap_diff="$(diff <(printf '%s\n' "$snap_before") <(printf '%s\n' "$snap_after") || true)"
[ -z "$snap_diff" ] \
    && ok "unit directory matches pre-install snapshot (diff empty)" \
    || bad "unit directory diff non-empty" "$(printf '%s\n' "$snap_diff" | head -10)"

# Confirm no spira-* units remain in systemctl's view (daemon-reload was called by
# uninstall.sh, so units no longer appear in list-unit-files).
spira_units_after="$("${CEXEC[@]}" "$CNAME" bash -c \
    'systemctl --user list-unit-files --no-legend 2>/dev/null \
     | awk '"'"'{print $1}'"'"' | grep "^spira-" || true')"
[ -z "$spira_units_after" ] \
    && ok "no spira-* units remain in systemctl list-unit-files" \
    || bad "spira-* units remain after uninstall" "$spira_units_after"

# ===========================================================================
echo
echo "stray sweep positive control — plant a unit, confirm STRAY is reported:"
# ===========================================================================
# Re-install under world.halted so units are enabled but not started.
"${CEXEC[@]}" "$CNAME" bash -c \
    "mkdir -p '${SPIRA_RUN_CTR}' && touch '${SPIRA_RUN_CTR}/world.halted'" >/dev/null
"${CEXEC[@]}" \
    -e "SPIRA_INSTALL_FORCE=1" \
    "${SUPERVISE_ENV[@]}" \
    "$CNAME" bash /workspace/systemd/install.sh >/dev/null 2>&1
iszero "re-install for stray test exits 0" "$?"

# Plant a unit not present in the owned manifest. uninstall.sh's stray sweep walks
# UNITDIR after the manifest-driven removal; any spira-* file that remains and is
# not in the manifest is reported as "STRAY".
"${CEXEC[@]}" "$CNAME" bash -c \
    'printf "[Unit]\nDescription=stray\n" \
     > "$HOME/.config/systemd/user/spira-legacy-stray.service"' >/dev/null
iszero "stray unit planted" "$?"

stray_out="$("${CEXEC[@]}" "$CNAME" \
    bash /workspace/spira/uninstall.sh --yes 2>&1)"
iszero "uninstall.sh --yes (with stray) exits 0" "$?"
want "stray sweep reports STRAY"          "STRAY"                        "$stray_out"
want "stray sweep names the planted unit" "spira-legacy-stray.service"   "$stray_out"

# ===========================================================================
echo
tl_summary
