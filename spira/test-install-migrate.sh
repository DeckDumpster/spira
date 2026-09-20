#!/usr/bin/env bash
#
# test-install-migrate.sh — install.sh migrates un-suffixed legacy units to
# per-instance naming when they are present.
#
#   ./test-install-migrate.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. MIGRATION ORDERING: when legacy (un-suffixed) spira-* units are present,
#    install.sh disables them BEFORE enabling per-instance units — no window
#    where both sets are enabled simultaneously.
# 2. MIGRATION SCOPE: install.sh disables the spira-* units that have per-instance
#    counterparts (not shared units like cockpit-ensure, concierge, beads-push).
# 3. CLEAN INSTALL: when no legacy units are present, no spurious disable calls
#    report success — a fresh-box install produces no migration output.
# 4. --NO-MIGRATE-WATCHERS: skips only watcher loops; sentinel is still migrated.
#
# A THIN PASS-THROUGH LOGGER records every systemctl call in order and execs
# real systemctl. The ordering assertion reads that log top-to-bottom. Legacy
# unit files are pre-installed in the user unit directory. The wrapper tracks
# which units plant_legacy enabled; a disable call returns 0 iff the unit was
# previously enabled through the wrapper — parallel suites may have already run
# _migrate_legacy on the same legacy names and removed them from real systemd.
#
# SCAR: the fixture's cp list omitted suite-covers.sh after sp-dt8u added it to
# lib.sh; lib.sh failed at source time before any assertion ran. A real install
# carries no fixture and the cause cannot exist.
#
# SKIP CONDITION: XDG_RUNTIME_DIR is not /run/user/1001 (suite must run inside
# the testenv container as spirauser) or user systemd is not responding.
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
# before <label> <must-precede> <must-follow> <log>
before() {
    local label="$1" na="$2" nb="$3" log="$4"
    local lno=0 lno_a=0 lno_b=0 line
    while IFS= read -r line; do
        lno=$((lno+1))
        [[ "$line" == *"$na"* ]] && lno_a=$lno
        [[ "$line" == *"$nb"* ]] && { lno_b=$lno; break; }
    done <<< "$log"
    if [ "$lno_a" -gt 0 ] && [ "$lno_b" -gt 0 ] && [ "$lno_a" -lt "$lno_b" ]; then
        ok "$label"
    else
        bad "$label" "[$na] (line $lno_a) must precede [$nb] (line $lno_b)"
    fi
}

echo "test-install-migrate.sh"

[ "${XDG_RUNTIME_DIR:-}" = "/run/user/1001" ] || {
    printf 'SKIP test-install-migrate.sh: not running as spirauser inside testenv container\n' >&2
    exit 77
}
# hermetic-ok: SKIP check — exits 77 when not inside the testenv container
systemctl --user status >/dev/null 2>&1 || {
    printf 'SKIP test-install-migrate.sh: user systemd not running inside container\n' >&2
    exit 77
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

SPIRA_RUN_DIR="$TMP/run"
DEST="$HOME/.config/systemd/user"
WATCHERS="$TMP/watchers"
mkdir -p "$SPIRA_RUN_DIR" "$DEST"
touch "$SPIRA_RUN_DIR/world.halted"
printf '# empty\n' > "$WATCHERS"

# PARALLEL-SAFE INSTANCE NAME. This suite shares the real DEST directory with
# other container-first suites (test-install-instance.sh, test-install-paths.sh).
# test-install-instance.sh uses "test" as its instance. The cleanup loops below
# delete all spira-*-${_INST}.* files; using a distinct name prevents those loops
# from deleting test-install-instance.sh's units when both suites run in parallel.
_INST="mig"

# Thin pass-through logger.
SCTL_LOG="$TMP/systemctl.log"
export SCTL_LOG
mkdir -p "$TMP/bin"
cat > "$TMP/bin/systemctl" << 'SCTL'
#!/bin/sh
printf '%s\n' "$*" >> "$SCTL_LOG"
# PARALLEL-SAFE ENABLE/DISABLE TRACKING. Another parallel install.sh
# (test-install-instance.sh) may call `systemctl disable --now` on the legacy
# units this test plants — the file is gone by the time _migrate_legacy runs here.
# Track which units this wrapper enabled; a disable returns 0 iff the unit was
# explicitly enabled through this wrapper, regardless of real systemd state. This
# preserves the clean-install invariant (no plants → no 0-returning disables) while
# surviving the race.
_ENABLED="${SCTL_LOG}.enabled"
case "$*" in
    "--user enable "*)
        _u="${*#*--user enable }"; printf '%s\n' "$_u" >> "$_ENABLED"
        exec /usr/bin/systemctl "$@" ;;
    "--user disable --now "*)
        _u="${*#*--user disable --now }"
        if grep -qxF "$_u" "$_ENABLED" 2>/dev/null; then
            grep -vxF "$_u" "$_ENABLED" 2>/dev/null > "${_ENABLED}.tmp" || true
            mv "${_ENABLED}.tmp" "$_ENABLED" 2>/dev/null || true
            /usr/bin/systemctl "$@" 2>/dev/null || true; exit 0
        fi
        exit 1 ;;
    *) exec /usr/bin/systemctl "$@" ;;
esac
SCTL
chmod +x "$TMP/bin/systemctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/bin/spira-supervise"
chmod +x "$TMP/bin/spira-supervise"

inst() {
    > "$SCTL_LOG"
    SCTL_LOG="$SCTL_LOG" \
    SPIRA_PATH="$TMP/bin" \
    SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$SPIRA_RUN_DIR" \
    SPIRA_WATCHERS="$WATCHERS" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    SPIRA_PROD= SPIRA_REPO_MAP=/nonexistent \
    SPIRA_INSTALL_FORCE=1 \
    SPIRA_SUPERVISE_BIN="$TMP/bin/spira-supervise" \
    bash "$HERE/../systemd/install.sh" "$_INST" "$@" 2>&1
}

# Helper: plant a legacy unit file in DEST and register it with the thin wrapper.
# Using the wrapper for enable registers the unit in the wrapper's tracking file so
# that _migrate_legacy's disable call returns 0 even if a parallel install.sh
# (test-install-instance.sh) has already disabled the unit via real systemd.
plant_legacy() {
    local u="$1"
    printf '[Unit]\nDescription=legacy %s\n[Service]\nExecStart=/bin/true\n[Install]\nWantedBy=default.target\n' \
        "$u" > "$DEST/$u"
    # hermetic-ok: container-first suite — registers unit with real systemd; SKIP guard exits 77
    systemctl --user daemon-reload 2>/dev/null
    "$TMP/bin/systemctl" --user enable "$u" 2>/dev/null || true  # hermetic-ok: container-first
}
remove_legacy() {
    local u
    for u in "$@"; do
        # hermetic-ok: container-first suite — cleans up real systemd state; SKIP guard exits 77
        systemctl --user disable --now "$u" 2>/dev/null || true
        rm -f "$DEST/$u"
    done
    # hermetic-ok: container-first suite — registers unit with real systemd; SKIP guard exits 77
    systemctl --user daemon-reload 2>/dev/null
}

# ==========================================================================
echo
echo "MIGRATION ORDERING — disable legacy units before enabling per-instance:"
# ==========================================================================

# DEST is empty of spira-*-${_INST}.* files — all units are new, so the enable loop
# calls systemctl for each one. This puts both disable and enable calls in the log,
# making the ordering assertion (before()) readable.
for f in "$DEST"/spira-*-${_INST}.*; do [ -e "$f" ] && rm -f "$f"; done
# hermetic-ok: container-first suite — reloads real systemd after unit cleanup; SKIP guard exits 77
systemctl --user daemon-reload 2>/dev/null

plant_legacy "spira-sentinel.service"
plant_legacy "spira-sentinel.timer"
plant_legacy "spira-ops.service"
plant_legacy "spira-ops.timer"

ord_out="$(inst)"
ord_rc=$?
ord_log="$(cat "$SCTL_LOG")"

iszero "ordering: install.sh exits 0 with legacy units present"  "$ord_rc"
want   "ordering: sentinel legacy unit appears in disable call"  \
       "spira-sentinel.service" "$ord_log"
want   "ordering: ops legacy unit appears in disable call"       \
       "spira-ops.service" "$ord_log"
want   "ordering: install output reports migration"              \
       "migrated" "$ord_out"

before "ordering: sentinel migrated before new unit starts" \
       "spira-sentinel.service" "spira-sentinel-${_INST}" "$ord_log"
before "ordering: ops migrated before new unit starts" \
       "spira-ops.service" "spira-ops-${_INST}" "$ord_log"

remove_legacy "spira-sentinel.service" "spira-sentinel.timer" \
              "spira-ops.service" "spira-ops.timer" 2>/dev/null || true

# ==========================================================================
echo
echo "MIGRATION SCOPE — shared units (cockpit-ensure, concierge, beads-push) excluded:"
# ==========================================================================

nowant "scope: cockpit-ensure.service not passed to disable" \
       "disable" "$(grep 'cockpit-ensure' "$SCTL_LOG" || true)"
nowant "scope: concierge.service not passed to disable" \
       "disable" "$(grep 'concierge' "$SCTL_LOG" || true)"
nowant "scope: beads-push.service not passed to disable" \
       "disable" "$(grep 'beads-push' "$SCTL_LOG" || true)"

# ==========================================================================
echo
echo "CLEAN INSTALL — no legacy units → disable returns 1, no migration output:"
# ==========================================================================

# No legacy units installed: systemctl disable for non-existent units returns 1
# (unit file not found), so _migrate_legacy's && short-circuits and no "migrated"
# message is printed.
for f in "$DEST"/spira-*-${_INST}.*; do [ -e "$f" ] && rm -f "$f"; done
# hermetic-ok: container-first suite — reloads real systemd after unit cleanup; SKIP guard exits 77
systemctl --user daemon-reload 2>/dev/null

clean_out="$(inst)"
clean_rc=$?

iszero  "clean install: exits 0 with no legacy units" "$clean_rc"
nowant  "clean install: no 'migrated' in output" "migrated" "$clean_out"

# ==========================================================================
echo
echo "--NO-MIGRATE-WATCHERS — skips only watcher loops; sentinel is still migrated:"
# ==========================================================================

printf 'testwatcher|daemon|/bin/true|\n' >> "$WATCHERS"

for f in "$DEST"/spira-*-${_INST}.*; do [ -e "$f" ] && rm -f "$f"; done
# hermetic-ok: container-first suite — reloads real systemd after unit cleanup; SKIP guard exits 77
systemctl --user daemon-reload 2>/dev/null

plant_legacy "spira-sentinel.service"
plant_legacy "spira-sentinel.timer"
plant_legacy "spira-watch-testwatcher.service"
# @ template-instance form
printf '[Unit]\nDescription=legacy watcher template\n[Service]\nExecStart=/bin/true\n[Install]\nWantedBy=default.target\n' \
    > "$DEST/spira-watch@.service"
# hermetic-ok: container-first suite — pre-plants @-template in real systemd; SKIP guard exits 77
systemctl --user daemon-reload 2>/dev/null
systemctl --user enable "spira-watch@testwatcher.service" 2>/dev/null || true  # hermetic-ok: container-first

skip_out="$(inst --no-migrate-watchers)"
skip_rc=$?
skip_log="$(cat "$SCTL_LOG")"

iszero  "--no-migrate-watchers: install.sh exits 0" "$skip_rc"
want    "--no-migrate-watchers: sentinel disable fires despite flag" \
        "disable --now spira-sentinel.service" "$skip_log"
nowant  "--no-migrate-watchers: watcher legacy unit not disabled" \
        "disable --now spira-watch-testwatcher" "$skip_log"
want    "--no-migrate-watchers: output notes the skip" \
        "no-migrate-watchers" "$skip_out"
want    "--no-migrate-watchers: sentinel per-instance unit still enabled" \
        "spira-sentinel-${_INST}" "$skip_log"

remove_legacy "spira-sentinel.service" "spira-sentinel.timer" \
              "spira-watch-testwatcher.service" "spira-watch@.service" 2>/dev/null || true

printf '# empty\n' > "$WATCHERS"

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
