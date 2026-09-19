#!/usr/bin/env bash
#
# test-install-unit-prune.sh — install.sh prunes non-watcher spira-* units that are
# installed but absent from the current manifest.
#
# PROPERTY UNDER TEST
# -------------------
# When install.sh runs and a non-watcher spira-*-<instance>.service or .timer is
# present in the installed unit directory but is NOT in the current UNITS manifest,
# install.sh disables, stops, and removes it. This mirrors the watcher prune loop.
#
# Without this pruning, a unit added by release B stays installed and crash-loops
# after a rollback to release A whose scripts it references do not exist.
#
# FAIL-FIRST: a unit is planted in DEST before the pruning loop existed; the suite
# verifies it fires. Against the unfixed tree (no prune loop) the unit stays and the
# assertion fails.
#
# SKIP CONDITION: must run inside the testenv container as spirauser.
#
# covers: systemd/install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }

echo "test-install-unit-prune.sh"

[ "${XDG_RUNTIME_DIR:-}" = "/run/user/1001" ] || {
    printf 'SKIP test-install-unit-prune.sh: not running as spirauser inside testenv container\n' >&2
    exit 77
}
# hermetic-ok: SKIP check — exits 77 when not inside the testenv container
systemctl --user status >/dev/null 2>&1 || {
    printf 'SKIP test-install-unit-prune.sh: user systemd not running inside container\n' >&2
    exit 77
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

SPIRA_RUN_DIR="$TMP/run"
DEST="$HOME/.config/systemd/user"
mkdir -p "$SPIRA_RUN_DIR" "$DEST"

FAKE_ORIGIN="$TMP/origin.git"
FAKE_REPO="$TMP/repo"
git init -q --bare -b main "$FAKE_ORIGIN" 2>/dev/null
git init -q -b main "$FAKE_REPO" 2>/dev/null
git -C "$FAKE_REPO" config user.email t@t
git -C "$FAKE_REPO" config user.name test
printf 'seed\n' > "$FAKE_REPO/f"
git -C "$FAKE_REPO" add f
git -C "$FAKE_REPO" commit -qm "seed" 2>/dev/null
git -C "$FAKE_REPO" remote add origin "$FAKE_ORIGIN"
git -C "$FAKE_REPO" push -q origin main 2>/dev/null
git -C "$FAKE_REPO" fetch -q origin 2>/dev/null
git -C "$FAKE_REPO" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main

WATCHERS="$TMP/watchers"
printf '# empty\n' > "$WATCHERS"

FIXTURE="$TMP/harness"
mkdir -p "$FIXTURE/systemd" "$FIXTURE/spira" "$FIXTURE/cockpit"
for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer; do
    [ -e "$f" ] || continue
    ln -s "$f" "$FIXTURE/systemd/$(basename "$f")"
done
ln -s "$HERE/../systemd/install.sh" "$FIXTURE/systemd/install.sh"
ln -s "$HERE/../systemd/units.sh"   "$FIXTURE/systemd/units.sh"
for f in conf.sh watchd.sh lib.sh owned.sh install-session-hook.sh suite-covers.sh; do
    [ -e "$HERE/$f" ] && ln -s "$HERE/$f" "$FIXTURE/spira/$f"
done
printf '# empty\n' > "$FIXTURE/spira/repo-map.example"
printf '# empty\n' > "$FIXTURE/spira/watchers"
ln -sf "$WATCHERS" "$FIXTURE/spira/watchers"

inst() {
    HOME="$TMP/home" \
    SPIRA_HOME="$FIXTURE/spira" \
    SPIRA_REPO="$FAKE_REPO" \
    SPIRA_RUN="$SPIRA_RUN_DIR" \
    SPIRA_DB="$TMP/db" \
    SPIRA_COCKPIT="$TMP/cockpit" \
    SPIRA_DOLT_DATA= \
    SPIRA_TESTDB_DATA= \
    SPIRA_PROD= \
    SPIRA_REPO_MAP=/nonexistent \
    SPIRA_CONF=/nonexistent \
    SPIRA_INSTALL_FORCE=1 \
    SPIRA_WATCHERS="$WATCHERS" \
    bash "$FIXTURE/systemd/install.sh" test 2>&1
}

# ==========================================================================
echo
echo "PRUNE — orphaned non-watcher spira-*-test.service is disabled and removed:"
# ==========================================================================

# FAIL-FIRST: plant the orphan unit and verify install.sh notices it.
# Choosing a name that looks like a real spira unit added by a newer release.
ORPHAN_UNIT="spira-mail-extra-test.service"
cat > "$DEST/$ORPHAN_UNIT" <<'EOF'
[Unit]
Description=orphaned extra unit from a newer release
[Service]
ExecStart=/bin/true
[Install]
WantedBy=default.target
EOF
# hermetic-ok: container-first suite — pre-plants unit in real systemd; SKIP guard exits 77
systemctl --user daemon-reload
systemctl --user enable "$ORPHAN_UNIT" 2>/dev/null || true

# FAIL-FIRST: verify the unit is installed and enabled before the prune runs.
_pre_state="$(systemctl --user is-enabled "$ORPHAN_UNIT" 2>/dev/null || true)"
if [ "$_pre_state" = "enabled" ]; then
    ok "fail-first: orphan unit is enabled before prune"
else
    bad "fail-first: orphan unit is enabled before prune" "state=$_pre_state"
fi

if [ -f "$DEST/$ORPHAN_UNIT" ]; then
    ok "fail-first: orphan unit file exists before prune"
else
    bad "fail-first: orphan unit file exists before prune" "file missing"
fi

prune_out="$(inst)"
prune_rc=$?

iszero "prune: install.sh exits 0 when pruning an orphan non-watcher unit" "$prune_rc"
want   "prune: orphan unit named in prune output" "$ORPHAN_UNIT" "$prune_out"
want   "prune: output says 'pruned'" "pruned" "$prune_out"

_post_state="$(systemctl --user is-enabled "$ORPHAN_UNIT" 2>/dev/null || true)"
if [ "$_post_state" != "enabled" ]; then
    ok "prune: orphan unit is no longer enabled after install"
else
    bad "prune: orphan unit is no longer enabled after install" \
        "still enabled — prune loop did not disable it"
fi

if [ ! -f "$DEST/$ORPHAN_UNIT" ]; then
    ok "prune: orphan unit file removed from DEST"
else
    bad "prune: orphan unit file removed from DEST" \
        "file still exists — prune loop did not remove it"
fi

# ==========================================================================
echo
echo "PRUNE — manifest unit is NOT pruned (positive guard):"
# ==========================================================================

# A unit that IS in the manifest must survive re-install.
# spira-sentinel-test.service is always in UNITS; verify it is not pruned.
if [ -f "$DEST/spira-sentinel-test.service" ]; then
    ok "guard: spira-sentinel-test.service present after install (not pruned)"
else
    bad "guard: spira-sentinel-test.service present after install" "file missing"
fi

# ==========================================================================
echo
echo "PRUNE — timer orphan is also pruned:"
# ==========================================================================

ORPHAN_TIMER="spira-mail-extra-test.timer"
cat > "$DEST/$ORPHAN_TIMER" <<'EOF'
[Unit]
Description=orphaned timer from a newer release
[Timer]
OnCalendar=*:0/5
[Install]
WantedBy=timers.target
EOF
systemctl --user daemon-reload
systemctl --user enable "$ORPHAN_TIMER" 2>/dev/null || true

timer_prune_out="$(inst)"
timer_prune_rc=$?

iszero "timer-prune: install.sh exits 0 when pruning an orphan timer" "$timer_prune_rc"
want   "timer-prune: orphan timer named in prune output" "$ORPHAN_TIMER" "$timer_prune_out"

if [ ! -f "$DEST/$ORPHAN_TIMER" ]; then
    ok "timer-prune: orphan timer file removed from DEST"
else
    bad "timer-prune: orphan timer file removed from DEST" "file still present"
fi

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
