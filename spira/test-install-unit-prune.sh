#!/usr/bin/env bash
#
# test-install-unit-prune.sh — install.sh prunes non-watcher spira-* units that are
# installed but absent from the current manifest (mirrors the watcher prune loop).
#
# PROPERTY UNDER TEST
# -------------------
# When install.sh runs and a non-watcher spira-*-<instance>.service or .timer is
# present in the installed unit directory but absent from UNITS, install.sh disables,
# stops, and removes it. Without this, a unit added by release B stays installed
# and crash-loops after a rollback to release A.
#
# FAIL-FIRST: a unit is planted in DEST before the run; the suite verifies that
# disable --now is called on it. Against the unfixed tree (no prune loop) the unit
# survives and the assertion fails.
#
# SKIP CONDITION: XDG_RUNTIME_DIR is not /run/user/1001 (must run inside the
# testenv container as spirauser) or user systemd is not responding.
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
    printf 'SKIP test-install-unit-prune.sh: user systemd not responding\n' >&2
    exit 77
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

SPIRA_RUN_DIR="$TMP/run"
DEST="$HOME/.config/systemd/user"
mkdir -p "$SPIRA_RUN_DIR" "$DEST"

# Halt the world so install.sh skips the "all enabled units are active" end check.
touch "$SPIRA_RUN_DIR/world.halted"

# Thin pass-through logger: records every systemctl call then execs the real binary.
SCTL_LOG="$TMP/systemctl.log"
mkdir -p "$TMP/bin"
cat > "$TMP/bin/systemctl" << 'SCTL'
#!/bin/sh
printf '%s\n' "$*" >> "$SCTL_LOG"
exec /usr/bin/systemctl "$@"
SCTL
chmod +x "$TMP/bin/systemctl"

WATCHERS="$TMP/watchers"
printf '# empty\n' > "$WATCHERS"

# inst [extra-env...] — run install.sh for the 'test' instance.
# SPIRA_PATH prepends the logger dir to PATH (conf.sh rebuilds PATH with it).
inst() {
    > "$SCTL_LOG"
    SCTL_LOG="$SCTL_LOG" \
    SPIRA_PATH="$TMP/bin" \
    SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$SPIRA_RUN_DIR" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    SPIRA_PROD= SPIRA_REPO_MAP=/nonexistent \
    SPIRA_INSTALL_FORCE=1 \
    SPIRA_WATCHERS="$WATCHERS" \
    "$@" \
    bash "$HERE/../systemd/install.sh" test 2>&1
}

# Pre-seed DEST so install.sh sees existing unit files and skips the daemon-reload
# and restart for unchanged units. Uses --render to get the content install.sh would write.
rendered="$(SCTL_LOG="$SCTL_LOG" SPIRA_PATH="$TMP/bin" SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$SPIRA_RUN_DIR" SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    SPIRA_PROD= SPIRA_REPO_MAP=/nonexistent \
    SPIRA_INSTALL_FORCE=1 SPIRA_WATCHERS="$WATCHERS" \
    bash "$HERE/../systemd/install.sh" test --render 2>&1)"
render_rc=$?
if [ "$render_rc" != 0 ]; then
    printf 'fixture: install.sh --render failed (rc=%s)\n' "$render_rc"
    printf '%s\n' "$rendered"
    exit 1
fi
current_unit=""
while IFS= read -r line; do
    if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
        current_unit="${BASH_REMATCH[1]}"; > "$DEST/$current_unit"
    elif [ -n "$current_unit" ]; then
        printf '%s\n' "$line" >> "$DEST/$current_unit"
    fi
done <<< "$rendered"

# Run a clean install to establish the baseline (units pre-seeded above).
inst_out="$(inst)"
inst_rc=$?
if [ "$inst_rc" != 0 ]; then
    printf 'fixture: baseline install failed (rc=%s)\n' "$inst_rc"
    printf '%s\n' "$inst_out"
    exit 1
fi

# ==========================================================================
echo
echo "PRUNE — orphaned non-watcher spira-*-test.service is disabled and removed:"
# ==========================================================================

ORPHAN_UNIT="spira-mail-extra-test.service"

# FAIL-FIRST: plant the orphan unit and verify it is known to systemd before pruning.
cat > "$DEST/$ORPHAN_UNIT" <<'EOF'
[Unit]
Description=orphaned extra unit from a newer release
[Service]
ExecStart=/bin/true
[Install]
WantedBy=default.target
EOF
# hermetic-ok: container-first suite — enables unit in real systemd; SKIP guard exits 77
systemctl --user daemon-reload
systemctl --user enable "$ORPHAN_UNIT" 2>/dev/null || true

if [ -f "$DEST/$ORPHAN_UNIT" ]; then
    ok "fail-first: orphan unit file is in DEST before prune"
else
    bad "fail-first: orphan unit file is in DEST before prune" "file not found"
fi

prune_out="$(inst)"
prune_rc=$?
prune_log="$(cat "$SCTL_LOG")"

iszero "prune: install.sh exits 0 when pruning an orphan" "$prune_rc"
want   "prune: disable called on orphan"            "disable" "$prune_log"
want   "prune: orphan unit named in disable call"   "$ORPHAN_UNIT" "$prune_log"
want   "prune: output says 'pruned'"                "pruned" "$prune_out"
want   "prune: orphan unit named in prune output"   "$ORPHAN_UNIT" "$prune_out"

_post="$(systemctl --user is-enabled "$ORPHAN_UNIT" 2>/dev/null || true)"
if [ "$_post" != "enabled" ]; then
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

if [ -f "$DEST/spira-sentinel-test.service" ]; then
    ok "guard: spira-sentinel-test.service present after install (not pruned)"
else
    bad "guard: spira-sentinel-test.service present after install" "file missing"
fi

# ==========================================================================
echo
echo "PRUNE — watcher units are NOT pruned by the non-watcher loop:"
# ==========================================================================

# Plant a fake watcher unit that the watcher prune loop would handle. Verify
# the non-watcher prune loop skips it (no double-prune).
WATCHER_UNIT="spira-watch-fakewatch-test.service"
cat > "$DEST/$WATCHER_UNIT" <<'EOF'
[Unit]
Description=fake watcher unit
[Service]
ExecStart=/bin/true
[Install]
WantedBy=default.target
EOF
systemctl --user daemon-reload
systemctl --user enable "$WATCHER_UNIT" 2>/dev/null || true

> "$SCTL_LOG"
prune2_out="$(inst)"
prune2_log="$(cat "$SCTL_LOG")"

# The watcher prune handles spira-watch-*; the non-watcher prune must skip it.
# (The watcher prune will also disable it since it is not in the manifest.)
# Either way: verify the non-watcher prune did not emit a second 'pruned' line for it.
nowant "watcher-skip: non-watcher prune loop did not emit pruned for watcher unit" \
       "pruned    $WATCHER_UNIT" "$prune2_out"

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
