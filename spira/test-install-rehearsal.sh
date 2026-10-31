#!/usr/bin/env bash
# test-install-rehearsal.sh — end-to-end install → assert → uninstall → clean-state cycle
#
# REAL SYSTEMD vs STUBS:
#   Real systemd  — unit files installed to ~/.config/systemd/user/ (real systemctl --user),
#                   sentinel timer started (real user session manager)
#   Stub          — bd (container has no bd; stub created as spirauser inside the container
#                   at /tmp/spira-stubs/bd satisfies conf.sh schema check, doctor
#                   fatal-binary check, and ready.sh database query)
#   Stub          — loom, by name on PATH (ready.sh; SPIRA_LOOM_PROBE then handles the
#                   actual probe call)
#   Stub          — SPIRA_LOOM_PROBE (script prints "200 5ms" in place of HTTP call)
#   Stub          — the fake prod root's bin/ (the units' ExecStart targets): bare exit-0
#                   scripts satisfy install.sh's executable-target refusal without paying a
#                   Rust build inside the container
#   Env override  — SPIRA_OPERATED=0 (this rehearsal has nobody at the console; doctor's
#                   operator-channel check is documented to WARN rather than FAIL here)
#   Warn path     — tmux (absent in container; layout.sh pane check WARNs, not FAILs)
#   NOT PROVEN    — filed bead reaches ready + sentinel.sh --report names it
#                   (operational bd+database required for bead filing; skipped here)
#   NOT PROVEN    — cockpit collector writes a snapshot (spira-cockpit-prod.service is
#                   enabled, like most units here, but never started; ready.sh's cockpit
#                   check FAILs on that alone, so it is asserted around rather than as
#                   part of the overall exit code)
#
# The stray-unit sweep, with its positive control, is test-install-rehearsal-stray.sh: it needs
# no install, so it runs beside this suite instead of behind it.
#
# SKIP CONDITION: no podman on PATH, or user systemd not available in the container.
#
# runtime: ~2m
# tier: T3
# bare-host: builds its own podman containers; the corpus container has no podman
# covers: spira/lib-test-install-rehearsal.sh spira/podman-exec-diag.sh install/src/bin/install.rs spira/uninstall.sh spira/configure.sh testenv/src/container.rs spira/testenv/Containerfile UC-instance-lifecycle-40
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM
. "$HERE/podman-exec-diag.sh"
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }
# Exit 3 is "installed but not ready": no cockpit collector runs in this container.
installed() { { [ "$2" = 0 ] || [ "$2" = 3 ]; } && ok "$1" || bad "$1" "exit $2"; }

echo "test-install-rehearsal.sh"

. "$HERE/lib-test-install-rehearsal.sh"

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
# not the base branch) and the live-dwarves guard.
"${CEXEC[@]}" "$CNAME" bash -c \
    "mkdir -p '${REH_RUN_CTR}' && touch '${REH_RUN_CTR}/world.halted'" >&2
iszero "world.halted created" "$?"

# A fresh host exports neither SPIRA_TOML nor SPIRA_RELEASES: install runs from the release it
# ships in (doctor beside it, no Spira directory on the inherited PATH) and finds the config
# configure wrote at its default path.
podman_exec_diag "$CNAME" "${INSTALL_EXEC[@]}" \
    -e "PATH=${STUBS_CTR}:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    -e "SPIRA_INSTALL_FORCE=1" \
    -e "SPIRA_INSTALL_LC_STORE_CONSIDERED=1" \
    -e "SPIRA_INSTALL_AERC_CONSIDERED=1" \
    "$CNAME" bash -c 'unset SPIRA_TOML SPIRA_RELEASES; exec /tmp/spira-prod/bin/spira-install --skip-build' >&2
installed "install completes with SPIRA_TOML and SPIRA_RELEASES both unset" "$?"

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

# Remove world.halted and start the sentinel timer so ready.sh sees it as active.
"${CEXEC[@]}" "$CNAME" bash -c \
    "rm -f '${REH_RUN_CTR}/world.halted'" >&2
iszero "world.halted removed" "$?"

"${CEXEC[@]}" "$CNAME" bash -c \
    'systemctl --user start spira-sentinel-prod.timer' >&2
iszero "sentinel timer started" "$?"

active_out="$("${CEXEC[@]}" "$CNAME" \
    bash -c 'systemctl --user is-active spira-sentinel-prod.timer 2>&1')"
want "sentinel timer active" "active" "$active_out"

# 3. World not halted — stamp file is gone.
"${CEXEC[@]}" "$CNAME" bash -c \
    "[ ! -f '${REH_RUN_CTR}/world.halted' ]" \
    && ok "world not halted (stamp absent)" \
    || bad "world not halted" "world.halted still present after removal"

# 4. doctor exits 0 — the stub bd/store fixture created above satisfies its
#    runtime-health checks (store reachable, no failed units, snapshot warns but
#    does not FAIL since the collector has not run yet).
# `doctor` is a compiled binary now (sp-yyk47), resolved by bare name on the container's
# own PATH like every other release tool (sp-gypjk) — never a `bash .../doctor.sh` path.
doc_out="$("${INSTALL_EXEC[@]}" "$CNAME" doctor 2>&1)"
doc_rc=$?
iszero "doctor exits 0" "$doc_rc"
nowant "doctor: no FAIL at all" "FAIL" "$doc_out"

# 5. ready.sh with stubs. The stub loom is on PATH; SPIRA_LOOM_PROBE returns "200 5ms".
#    sentinel.sh --report renders WARN (no open beads) and seed.sh --list renders
#    WARN (no statutes in the stub db) — both are WARNs, not FAILs. The cockpit
#    collector check is a known FAIL (see NOT PROVEN, header) since nothing here
#    starts spira-cockpit-prod.service, so specific lines are asserted instead of
#    the overall exit code.
ready_out="$("${INSTALL_EXEC[@]}" \
    -e "SPIRA_LOOM_PROBE=${STUBS_CTR}/loom-probe" \
    -e "SPIRA_COCKPIT_FIRST_WRITE_WAIT_S=1" \
    "$CNAME" bash /workspace/spira/ready.sh 2>&1)"
want "ready.sh: sentinel timer active" "pass  sentinel timer active" "$ready_out"
want "ready.sh: world not halted"     "pass  world not halted"       "$ready_out"
want "ready.sh: database readable"    "pass  database readable"      "$ready_out"
want "ready.sh: loom probe passes"    "loom answers 200"              "$ready_out"
want "ready.sh: dwarf warm base is a writable directory" "pass  dwarf warm base" "$ready_out"
nowant "ready.sh: dwarf warm base is not missing" "dwarf warm base .* is not a writable directory" "$ready_out"

# ===========================================================================
echo
echo "uninstall — remove the installation:"
# ===========================================================================
"${INSTALL_EXEC[@]}" "$CNAME" \
    bash /workspace/spira/uninstall.sh --yes >/dev/null 2>&1
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

echo
tl_summary
