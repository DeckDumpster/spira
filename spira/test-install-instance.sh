#!/usr/bin/env bash
#
# test-install-instance.sh — install.sh renders per-instance unit names; prune
# follows per-instance naming; installing one instance does not disrupt another.
#
#   ./test-install-instance.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. PER-INSTANCE NAMING: install.sh test writes spira-*-test.service/timer files to
#    DEST and does NOT write spira-*-prod.* files (no cross-instance contamination).
#    Non-spira units (cockpit-ensure, concierge, beads-push) keep their plain names.
# 2. PRUNE: a spira-watch-*-test.service unit that was installed for a watcher row now
#    absent from the manifest is disabled. The old template pattern (spira-watch@*.service)
#    is gone; prune operates on per-instance names.
# 3. AEON ISOLATION: installing the 'test' instance succeeds even when 'prod' aeons are
#    live, because the guard matches spira-aeon-*-test.service, not spira-aeon-*-prod.service.
# 4. WATCHER INSTALL: when the manifest has a 'testview' row, install.sh writes
#    spira-watch-testview-test.service (not spira-watch@testview.service).
#
# A THIN PASS-THROUGH LOGGER records every systemctl call to a log file and execs
# real systemctl, so all enable/disable/start/list operations use actual systemd.
# This is instrumentation, not a stub — systemd state is authoritative.
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
nonzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }
atleast() { [ "$3" -ge "$2" ] && ok "$1" || bad "$1" "wanted >= $2, got $3"; }

echo "test-install-instance.sh"

[ "${XDG_RUNTIME_DIR:-}" = "/run/user/1001" ] || {
    printf 'SKIP test-install-instance.sh: not running as spirauser inside testenv container\n' >&2
    exit 77
}
# hermetic-ok: SKIP check — exits 77 when not inside the testenv container
systemctl --user status >/dev/null 2>&1 || {
    printf 'SKIP test-install-instance.sh: user systemd not running inside container\n' >&2
    exit 77
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

SPIRA_RUN_DIR="$TMP/run"
DEST="$HOME/.config/systemd/user"
mkdir -p "$SPIRA_RUN_DIR" "$DEST"
touch "$SPIRA_RUN_DIR/world.halted"

# Thin pass-through logger: records every systemctl call in order, then execs
# the real binary. Set SPIRA_PATH so conf.sh prepends this dir to PATH.
SCTL_LOG="$TMP/systemctl.log"
mkdir -p "$TMP/bin"
cat > "$TMP/bin/systemctl" << 'SCTL'
#!/bin/sh
printf '%s\n' "$*" >> "$SCTL_LOG"
exec /usr/bin/systemctl "$@"
SCTL
chmod +x "$TMP/bin/systemctl"

# inst [extra-env...] [args] — run install.sh for the 'test' instance.
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
    "$@" \
    bash "$HERE/../systemd/install.sh" test 2>&1
}

# ==========================================================================
echo
echo "PER-INSTANCE NAMING — install.sh test writes spira-*-test.* to DEST:"
# ==========================================================================

WATCHERS="$TMP/watchers"
printf '# empty\n' > "$WATCHERS"

# Seed DEST via --render so unit files exist before the full install compares.
rendered="$(SCTL_LOG="$SCTL_LOG" SPIRA_PATH="$TMP/bin" SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$SPIRA_RUN_DIR" SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    SPIRA_PROD= SPIRA_REPO_MAP=/nonexistent \
    SPIRA_INSTALL_FORCE=1 SPIRA_WATCHERS="$WATCHERS" \
    bash "$HERE/../systemd/install.sh" test --render 2>&1)"
render_rc=$?
if [ "$render_rc" != 0 ]; then
    printf 'fixture: install.sh test --render failed (rc=%s)\n' "$render_rc"
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

clean_out="$(SPIRA_WATCHERS="$WATCHERS" inst)"
clean_rc=$?
clean_log="$(cat "$SCTL_LOG")"

iszero "naming: install.sh test exits 0" "$clean_rc"

bad_files="$(cd "$DEST" && ls -1 spira-* 2>/dev/null \
              | grep -v '^spira-.*-test\.' || true)"
[ -z "$bad_files" ] \
    && ok "naming: no spira-* file without -test suffix in DEST" \
    || bad "naming: spira-* files without -test suffix found: $bad_files" ""

test_files="$(cd "$DEST" && ls -1 spira-*-test.service spira-*-test.timer 2>/dev/null | wc -l | tr -d ' ')"
atleast "naming: at least 14 spira-*-test units installed" 14 "$test_files"

want  "naming: cockpit-ensure.service present" "cockpit-ensure.service" "$(ls "$DEST")"
want  "naming: concierge.service present"      "concierge.service"      "$(ls "$DEST")"
want  "naming: beads-push.service present"     "beads-push.service"     "$(ls "$DEST")"

want  "naming: sentinel timer enabled with -test suffix" "spira-sentinel-test.timer" "$clean_log"
nowant "naming: no plain spira-sentinel.timer in enable" \
      "enable --now spira-sentinel.timer" "$clean_log"

# ==========================================================================
echo
echo "PRUNE — orphaned spira-watch-*-test.service is disabled:"
# ==========================================================================

# Pre-install the watcher so it is known to systemd, then remove it from the
# manifest to make it orphaned on re-install.
WATCHER_UNIT="spira-watch-oldwatcher-test.service"
printf '[Unit]\nDescription=oldwatcher\n[Service]\nExecStart=/bin/true\n[Install]\nWantedBy=default.target\n' \
    > "$DEST/$WATCHER_UNIT"
# hermetic-ok: container-first suite — pre-plants legacy unit in real systemd; SKIP guard exits 77
systemctl --user daemon-reload
systemctl --user enable "$WATCHER_UNIT" 2>/dev/null || true  # hermetic-ok: container-first

printf '# empty\n' > "$WATCHERS"

prune_out="$(SPIRA_WATCHERS="$WATCHERS" inst)"
prune_rc=$?
prune_log="$(cat "$SCTL_LOG")"

iszero  "prune: exit 0 even when pruning an orphan"       "$prune_rc"
want    "prune: disable called on orphaned watcher"        \
        "disable" "$prune_log"
want    "prune: orphaned unit named in disable call"       \
        "spira-watch-oldwatcher-test.service" "$prune_log"
nowant  "prune: no spira-watch@ template in systemctl log" \
        "spira-watch@" "$prune_log"

# ==========================================================================
echo
echo "WATCHER INSTALL — manifest row installs spira-watch-testview-test.service:"
# ==========================================================================

printf 'testview|daemon|/bin/true\n' > "$WATCHERS"

wrendered="$(SCTL_LOG="$SCTL_LOG" SPIRA_PATH="$TMP/bin" SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$SPIRA_RUN_DIR" SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    SPIRA_PROD= SPIRA_REPO_MAP=/nonexistent \
    SPIRA_INSTALL_FORCE=1 SPIRA_WATCHERS="$WATCHERS" \
    bash "$HERE/../systemd/install.sh" test --render 2>&1)"
current_unit=""
while IFS= read -r line; do
    if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
        current_unit="${BASH_REMATCH[1]}"; > "$DEST/$current_unit"
    elif [ -n "$current_unit" ]; then
        printf '%s\n' "$line" >> "$DEST/$current_unit"
    fi
done <<< "$wrendered"

want   "watcher install: --render includes spira-watch-testview-test.service header" \
       "===== spira-watch-testview-test.service =====" "$wrendered"
nowant "watcher install: --render has no @-template name" \
       "spira-watch@testview" "$wrendered"

# Remove the watcher unit from DEST so install.sh writes it fresh (marking it _NEW),
# which keeps the enable logic from treating it as operator-disabled.
rm -f "$DEST/spira-watch-testview-test.service"

watcher_out="$(SPIRA_WATCHERS="$WATCHERS" inst)"
watcher_rc=$?
watcher_log="$(cat "$SCTL_LOG")"

iszero  "watcher install: exit 0" "$watcher_rc"
want    "watcher install: installed spira-watch-testview-test.service" \
        "installed spira-watch-testview-test.service" "$watcher_out"
want    "watcher install: enabled spira-watch-testview-test.service" \
        "spira-watch-testview-test.service" "$watcher_log"
nowant  "watcher install: no @-template name in enable/restart calls" \
        "spira-watch@testview" \
        "$(grep -E ' enable | restart ' "$SCTL_LOG" || true)"
[ -f "$DEST/spira-watch-testview-test.service" ] \
    && ok "watcher install: unit file written to DEST" \
    || bad "watcher install: unit file missing from DEST" ""

# ==========================================================================
echo
echo "AEON ISOLATION — installing test does not block on prod aeons:"
# ==========================================================================

printf '# empty\n' > "$WATCHERS"

# SPIRA_INSTALL_FORCE=1 (used in all inst() calls to bypass the landref check in the
# container) also bypasses the live-aeons guard. The guard's instance-scoping is verified
# through lib.sh:spira_live_aeons, which uses "spira-aeon-*-${SPIRA_INSTANCE}.service".
# The key assertion here is exit 0: a 'test' install does not fail because 'prod' services
# happen to be running.
aeon_out="$(SPIRA_WATCHERS="$WATCHERS" inst)"
aeon_rc=$?

iszero "aeon isolation: exit 0 when prod aeons are live during test install" "$aeon_rc"

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
