#!/usr/bin/env bash
# test-uninstall.sh — uninstall.sh: removes default tier, idempotent, instance-aware,
# stray-unit sweep reports a deliberately planted leftover.
#
#   ./test-uninstall.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. DEFAULT REMOVAL: units are stopped, disabled, and removed from UNITDIR;
#    linger is disabled; ~/.local/bin symlinks pointing to the harness are removed;
#    session hooks are removed from the agent settings file.
# 2. IDEMPOTENCY: a second run exits 0 with nothing to remove.
# 3. INSTANCE AWARENESS: refuses when multiple instances are installed and no
#    argument is given; accepts an explicit instance argument.
# 4. STRAY SWEEP: a unit file planted after owned.sh runs — not in the manifest —
#    is reported as a STRAY and not silently missed.
# 5. --purge: config and runtime directories are removed.
# 6. --dry-run: nothing is changed; exit 0.
# 7. PARTIAL INSTALL: absent artifacts do not cause non-zero exit.
# 8. --purge-database (G1): removes the beads database and its Dolt data only when the
#    operator types back the fake bd backend's bead count; a wrong count leaves it
#    intact with a non-zero exit; plain --purge never touches it.
#
# FAIL-FIRST: tested against the state BEFORE uninstall.sh existed to confirm
# the suite is not trivially green.
#
# tier: T2
# covers: spira/uninstall.sh spira/owned.sh systemd/install.sh UC-instance-lifecycle-38 UC-instance-lifecycle-39
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
REAL_COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
nonzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }
isfile()  { [ -f "$2" ] && ok "$1" || bad "$1" "expected file: $2"; }
isdir()   { [ -d "$2" ] && ok "$1" || bad "$1" "expected dir: $2"; }
nofile()  { [ ! -e "$2" ] && ok "$1" || bad "$1" "expected absent: $2"; }

echo "test-uninstall.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# A2's installed-release model runs install.sh from releases/current, not a git
# checkout — SPIRA_REPO is not a git repo here, and SPIRA_INSTALL_FORCE=1 skips
# the landref-currency check that only applies to a source checkout.
FAKE_REPO="$TMP/repo"
mkdir -p "$FAKE_REPO"

# ---------------------------------------------------------------------------
# Fixture: minimal harness tree with real scripts behind symlinks so that
# owned.sh can source conf.sh, watchd.sh, etc.
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
mkdir -p "$FIXTURE/systemd" "$FIXTURE/spira" "$FIXTURE/cockpit"

for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer; do
    [ -e "$f" ] || continue
    ln -s "$f" "$FIXTURE/systemd/$(basename "$f")"
done
ln -s "$HERE/../systemd/install.sh"  "$FIXTURE/systemd/install.sh"
ln -s "$HERE/../systemd/units.sh"    "$FIXTURE/systemd/units.sh"
for f in conf.sh watchd.sh lib.sh owned.sh install-session-hook.sh; do
    [ -e "$HERE/$f" ] && ln -s "$HERE/$f" "$FIXTURE/spira/$f"
done
# Link the real uninstall.sh so the test drives it.
ln -s "$HERE/uninstall.sh" "$FIXTURE/spira/uninstall.sh"
printf '# empty\n' > "$FIXTURE/spira/repo-map.example"
printf '# empty\n' > "$FIXTURE/spira/watchers"

# Stub install-intake.sh — alert drop-ins are not the focus here; we just need
# it to not fail.
printf '#!/usr/bin/env bash\necho "install-intake: $*"\n' > "$FIXTURE/spira/install-intake.sh"
chmod +x "$FIXTURE/spira/install-intake.sh"

# Stub cockpit/layout.sh: record calls; do not kill any real sessions.
mkdir -p "$FIXTURE/cockpit"
cat > "$FIXTURE/cockpit/layout.sh" <<'LAYOUT'
#!/usr/bin/env bash
printf 'layout.sh: %s\n' "$*" >> "${LAYOUT_LOG:-/dev/null}"
exit 0
LAYOUT
chmod +x "$FIXTURE/cockpit/layout.sh"

# Directories wired into the test environment.
DEST="$TMP/home/.config/systemd/user"
SPIRA_RUN_DIR="$TMP/run"
CONF_DIR="$TMP/home/.config/spira"
MOCK_BIN="$TMP/mock-bin"
LOCAL_BIN="$TMP/home/.local/bin"
LAYOUT_LOG="$TMP/layout.log"
mkdir -p "$DEST" "$SPIRA_RUN_DIR" "$CONF_DIR" "$MOCK_BIN" "$LOCAL_BIN"

MOCK_LOG="$TMP/systemctl.log"
LINGER_LOG="$TMP/loginctl.log"

# ---------------------------------------------------------------------------
# Mock systemctl — records calls; stop/disable always succeed.
# ---------------------------------------------------------------------------
cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${MOCK_LOG}"
case "$*" in
    *list-units*) printf '' ;;
    *is-active*)  printf 'inactive\n' ;;
    *daemon-reload*) ;;
    *) ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"

# Mock loginctl — records calls; show-user returns "Linger=yes" when flagged.
cat > "$MOCK_BIN/loginctl" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${LINGER_LOG}"
case "$*" in
    *show-user*Linger*) printf 'Linger=%s\n' "${MOCK_LINGER:-yes}" ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/loginctl"

# Mock tmux — session hooks check reads it; uninstall uses cockpit/layout.sh only.
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/tmux"
chmod +x "$MOCK_BIN/tmux"

# ---------------------------------------------------------------------------
# un [args] — run uninstall.sh in the controlled environment.
# Always uses the 'test' instance. Passes --yes to skip the confirmation prompt.
# ---------------------------------------------------------------------------
un() {
    > "$MOCK_LOG"; > "$LINGER_LOG"; > "$LAYOUT_LOG"
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$FIXTURE/spira" \
        "SPIRA_PROD=$FIXTURE/spira" \
        "SPIRA_REPO=$FAKE_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "SPIRA_INSTANCE=test" \
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
        "SPIRA_SYSTEMCTL=$MOCK_BIN/systemctl" \
        "SPIRA_LOGINCTL=$MOCK_BIN/loginctl" \
        "SPIRA_TMUX=$MOCK_BIN/tmux" \
        "MOCK_LOG=$MOCK_LOG" \
        "LINGER_LOG=$LINGER_LOG" \
        "LAYOUT_LOG=$LAYOUT_LOG" \
        "MOCK_LINGER=${MOCK_LINGER:-yes}" \
        bash "$FIXTURE/spira/uninstall.sh" test --yes "$@" 2>&1
}

# ---------------------------------------------------------------------------
# Seed DEST with per-instance unit files so uninstall has something to remove.
# Use the same rendering path as install tests: run --render, write the files.
# ---------------------------------------------------------------------------
_seed_units() {
    local rendered rc
    rendered="$(env -i \
        "PATH=$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$FIXTURE/spira" \
        "SPIRA_PROD=$FIXTURE/spira" \
        "SPIRA_REPO=$FAKE_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "SPIRA_INSTANCE=test" \
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
        "SPIRA_SYSTEMCTL=$MOCK_BIN/systemctl" \
        "SPIRA_INSTALL_FORCE=1" \
        bash "$FIXTURE/systemd/install.sh" test --render 2>&1)"
    rc=$?
    if [ "$rc" != 0 ]; then
        printf 'fixture: install.sh --render failed (rc=%s)\n' "$rc" >&2
        printf '%s\n' "$rendered" >&2
        return 1
    fi
    local current_unit=""
    while IFS= read -r line; do
        if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
            current_unit="${BASH_REMATCH[1]}"; > "$DEST/$current_unit"
        elif [ -n "$current_unit" ]; then
            printf '%s\n' "$line" >> "$DEST/$current_unit"
        fi
    done <<< "$rendered"
}

# ---------------------------------------------------------------------------
# Seed agent settings file with fake session hooks.
# ---------------------------------------------------------------------------
SETTINGS="$TMP/home/.claude/settings.json"
mkdir -p "$(dirname "$SETTINGS")"
_un_hook="$FIXTURE/spira/hooks/session.sh"
mkdir -p "$FIXTURE/spira/hooks"
printf '#!/usr/bin/env bash\nexit 0\n' > "$_un_hook"; chmod +x "$_un_hook"
cat > "$SETTINGS" <<JSON
{
  "hooks": {
    "SessionStart": [
      {"hooks": [{"type": "command", "command": "$_un_hook", "timeout": 10}]}
    ],
    "PostCompact": [
      {"hooks": [{"type": "command", "command": "$_un_hook", "timeout": 10}]}
    ]
  }
}
JSON

# Seed ~/.local/bin symlinks pointing into the harness.
ln -sf "$FIXTURE/cockpit/remote-cockpit" "$LOCAL_BIN/cockpit-remote" 2>/dev/null || \
    ln -sf "$FIXTURE/cockpit/layout.sh" "$LOCAL_BIN/cockpit-remote"

# ==========================================================================
echo
echo "DEFAULT REMOVAL — units, linger, symlinks, session hooks:"
# ==========================================================================

_seed_units || { printf 'fixture: seeding failed\n'; exit 1; }

_unit_count="$(ls -1 "$DEST"/*.service "$DEST"/*.timer 2>/dev/null | wc -l | tr -d ' ')"
[ "$_unit_count" -gt 0 ] || { printf 'fixture: no units seeded in DEST\n'; exit 1; }

out="$(un)"
rc=$?

iszero "default: exit 0" "$rc"

# Units must be gone from DEST.
_remaining="$(ls -1 "$DEST"/spira-*-test.service "$DEST"/spira-*-test.timer 2>/dev/null | wc -l | tr -d ' ')"
[ "$_remaining" -eq 0 ] \
    && ok  "default: unit files removed from DEST" \
    || bad "default: $( _remaining ) unit files remain in DEST" "$_remaining"

# systemctl stop and disable must have been called.
want "default: stop called on units"    "stop"    "$(cat "$MOCK_LOG")"
want "default: disable called on units" "disable" "$(cat "$MOCK_LOG")"

# Linger must have been disabled.
want "default: loginctl disable-linger called" "disable-linger" "$(cat "$LINGER_LOG")"

# ~/.local/bin/cockpit-remote symlink must be gone.
nofile "default: cockpit-remote symlink removed" "$LOCAL_BIN/cockpit-remote"

# Session hooks must be removed from settings.
_hook_count="$(python3 -c "
import json, sys
d = json.load(open('$SETTINGS'))
h = d.get('hooks', {})
print(sum(len(v) for v in h.values()))
" 2>/dev/null || echo 999)"
[ "$_hook_count" -eq 0 ] \
    && ok  "default: session hooks removed from settings" \
    || bad "default: session hooks not all removed (remaining entries: $_hook_count)" ""

# ==========================================================================
echo
echo "IDEMPOTENCY — second run exits 0 with nothing to remove:"
# ==========================================================================

out2="$(un)"
rc2=$?
iszero "idempotency: second run exits 0" "$rc2"

# ==========================================================================
echo
echo "INSTANCE AWARENESS — refuses multiple instances; accepts explicit:"
# ==========================================================================

# Plant two sentinel files for two instances to trigger the refusal.
> "$DEST/spira-sentinel-prod.service"
> "$DEST/spira-sentinel-test.service"

multi_out="$(env -i \
    "PATH=$PATH" \
    "HOME=$TMP/home" \
    SPIRA_CONF=/nonexistent \
    "SPIRA_PATH=$MOCK_BIN" \
    "SPIRA_RUN=$SPIRA_RUN_DIR" \
    "SPIRA_HOME=$FIXTURE/spira" \
    "SPIRA_PROD=$FIXTURE/spira" \
    "SPIRA_REPO=$FAKE_REPO" \
    "SPIRA_COCKPIT=$REAL_COCKPIT" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    "SPIRA_SYSTEMCTL=$MOCK_BIN/systemctl" \
    "SPIRA_LOGINCTL=$MOCK_BIN/loginctl" \
    "SPIRA_TMUX=$MOCK_BIN/tmux" \
    bash "$FIXTURE/spira/uninstall.sh" 2>&1)"
multi_rc=$?

nonzero "instance: refuses when multiple instances, no argument" "$multi_rc"
want    "instance: names the instances found" "prod" "$multi_out"
want    "instance: names the instances found" "test" "$multi_out"

# Explicit argument should be accepted even with multiple sentinels.
explicit_out="$(env -i \
    "PATH=$PATH" \
    "HOME=$TMP/home" \
    SPIRA_CONF=/nonexistent \
    "SPIRA_PATH=$MOCK_BIN" \
    "SPIRA_RUN=$SPIRA_RUN_DIR" \
    "SPIRA_HOME=$FIXTURE/spira" \
    "SPIRA_PROD=$FIXTURE/spira" \
    "SPIRA_REPO=$FAKE_REPO" \
    "SPIRA_COCKPIT=$REAL_COCKPIT" \
    "SPIRA_INSTANCE=test" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    "SPIRA_SYSTEMCTL=$MOCK_BIN/systemctl" \
    "SPIRA_LOGINCTL=$MOCK_BIN/loginctl" \
    "SPIRA_TMUX=$MOCK_BIN/tmux" \
    "MOCK_LINGER=yes" \
    bash "$FIXTURE/spira/uninstall.sh" test --yes 2>&1)"
explicit_rc=$?
iszero "instance: explicit 'test' arg accepted" "$explicit_rc"

# Clean up planted sentinels.
rm -f "$DEST/spira-sentinel-prod.service" "$DEST/spira-sentinel-test.service"

# ==========================================================================
echo
echo "STRAY SWEEP — planted leftover not in manifest is reported:"
# ==========================================================================

# Re-seed units so the removal pass runs and we get to the sweep.
_seed_units || { printf 'fixture: re-seed failed\n'; exit 1; }

# Plant a spira-* unit that owned.sh will never declare — simulating a file left
# by an older harness version.
STRAY_UNIT="$DEST/spira-legacy-shard-test.service"
printf '[Unit]\nDescription=stray legacy unit for test\n' > "$STRAY_UNIT"

stray_out="$(un)"
stray_rc=$?

iszero "sweep: exit 0 even when stray unit found" "$stray_rc"
want   "sweep: stray unit is reported" "STRAY" "$stray_out"
want   "sweep: stray unit name appears in report" "spira-legacy-shard-test.service" "$stray_out"

# The stray should NOT have been removed (report only, no silent deletion).
isfile "sweep: stray unit file NOT removed (report only)" "$STRAY_UNIT"
rm -f "$STRAY_UNIT"

# ==========================================================================
echo
echo "--purge — config and runtime directories removed:"
# ==========================================================================

_seed_units || { printf 'fixture: re-seed for purge failed\n'; exit 1; }
mkdir -p "$CONF_DIR"
printf 'SPIRA_INSTANCE=test\n' > "$CONF_DIR/spira.conf"
mkdir -p "$SPIRA_RUN_DIR/archive"

purge_out="$(env -i \
    "PATH=$PATH" \
    "HOME=$TMP/home" \
    SPIRA_CONF=/nonexistent \
    "SPIRA_PATH=$MOCK_BIN" \
    "SPIRA_RUN=$SPIRA_RUN_DIR" \
    "SPIRA_HOME=$FIXTURE/spira" \
    "SPIRA_PROD=$FIXTURE/spira" \
    "SPIRA_REPO=$FAKE_REPO" \
    "SPIRA_COCKPIT=$REAL_COCKPIT" \
    "SPIRA_INSTANCE=test" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    "SPIRA_SYSTEMCTL=$MOCK_BIN/systemctl" \
    "SPIRA_LOGINCTL=$MOCK_BIN/loginctl" \
    "SPIRA_TMUX=$MOCK_BIN/tmux" \
    "MOCK_LINGER=no" \
    bash "$FIXTURE/spira/uninstall.sh" test --yes --purge 2>&1)"
purge_rc=$?

iszero "purge: exit 0" "$purge_rc"
nofile "purge: config file removed" "$CONF_DIR/spira.conf"
[ ! -d "$SPIRA_RUN_DIR" ] \
    && ok  "purge: runtime dir removed" \
    || bad "purge: runtime dir still present" ""

# Recreate SPIRA_RUN_DIR so subsequent sub-tests work.
mkdir -p "$SPIRA_RUN_DIR"

# ==========================================================================
echo
echo "--dry-run — nothing changed:"
# ==========================================================================

_seed_units || { printf 'fixture: re-seed for dry-run failed\n'; exit 1; }
_unit_before="$(ls -1 "$DEST" 2>/dev/null | wc -l | tr -d ' ')"

dryrun_out="$(env -i \
    "PATH=$PATH" \
    "HOME=$TMP/home" \
    SPIRA_CONF=/nonexistent \
    "SPIRA_PATH=$MOCK_BIN" \
    "SPIRA_RUN=$SPIRA_RUN_DIR" \
    "SPIRA_HOME=$FIXTURE/spira" \
    "SPIRA_PROD=$FIXTURE/spira" \
    "SPIRA_REPO=$FAKE_REPO" \
    "SPIRA_COCKPIT=$REAL_COCKPIT" \
    "SPIRA_INSTANCE=test" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    "SPIRA_SYSTEMCTL=$MOCK_BIN/systemctl" \
    "SPIRA_LOGINCTL=$MOCK_BIN/loginctl" \
    "SPIRA_TMUX=$MOCK_BIN/tmux" \
    bash "$FIXTURE/spira/uninstall.sh" test --dry-run 2>&1)"
dryrun_rc=$?

_unit_after="$(ls -1 "$DEST" 2>/dev/null | wc -l | tr -d ' ')"

iszero "dry-run: exit 0"                     "$dryrun_rc"
want   "dry-run: reports DRY RUN"            "DRY RUN"  "$dryrun_out"
[ "$_unit_before" = "$_unit_after" ] \
    && ok  "dry-run: unit count unchanged ($_unit_before files)" \
    || bad "dry-run: unit count changed from $_unit_before to $_unit_after" ""

# ==========================================================================
echo
echo "PARTIAL INSTALL — absent artifacts do not cause non-zero exit:"
# ==========================================================================

# Wipe DEST entirely to simulate a partial install.
rm -rf "$DEST"
mkdir -p "$DEST"

partial_out="$(un)"
partial_rc=$?

iszero "partial: exit 0 when DEST is empty" "$partial_rc"

# ==========================================================================
echo
echo "--purge-database — removes the DB only on a matching typed-back count (gap G1):"
# ==========================================================================

# Fake bd backend: 'list --all --format=json' returns a fixed-length array so the
# right/wrong count is known ahead of time; anything else (conf.sh's own schema
# probe) is a no-op success.
cat > "$MOCK_BIN/bd" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"list --all --format=json"*) printf '%s' "${FAKE_BD_LIST_JSON:-[]}" ;;
    *) exit 0 ;;
esac
MOCK
chmod +x "$MOCK_BIN/bd"
FAKE_BD_LIST_JSON='[{"id":"a"},{"id":"b"},{"id":"c"}]'   # 3 beads

PURGEDB_DIR="$TMP/beads-db"
PURGEDB_DOLT="$TMP/dolt-data"
PURGEDB_TESTDB="$TMP/testdb-data"

_seed_purgedb() {
    rm -rf "$PURGEDB_DIR" "$PURGEDB_DOLT" "$PURGEDB_TESTDB"
    mkdir -p "$PURGEDB_DIR/.beads" "$PURGEDB_DOLT" "$PURGEDB_TESTDB"
}

# un_purgedb <typed-count> [uninstall.sh flags...] — same controlled environment as
# un(), plus the fake bd backend and a real .beads dir for the purge-database count.
un_purgedb() {
    local confirm="$1"; shift
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$FIXTURE/spira" \
        "SPIRA_PROD=$FIXTURE/spira" \
        "SPIRA_REPO=$FAKE_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "SPIRA_INSTANCE=test" \
        "SPIRA_DB=$PURGEDB_DIR" \
        "SPIRA_BD=$MOCK_BIN/bd" \
        "SPIRA_DOLT_DATA=$PURGEDB_DOLT" \
        "SPIRA_TESTDB_DATA=$PURGEDB_TESTDB" \
        "SPIRA_SYSTEMCTL=$MOCK_BIN/systemctl" \
        "SPIRA_LOGINCTL=$MOCK_BIN/loginctl" \
        "SPIRA_TMUX=$MOCK_BIN/tmux" \
        "MOCK_LOG=$MOCK_LOG" \
        "LINGER_LOG=$LINGER_LOG" \
        "LAYOUT_LOG=$LAYOUT_LOG" \
        "FAKE_BD_LIST_JSON=$FAKE_BD_LIST_JSON" \
        bash "$FIXTURE/spira/uninstall.sh" test --yes "$@" <<< "$confirm" 2>&1
}

# Wrong count: database, Dolt data and test Dolt data all left intact; non-zero exit.
_seed_purgedb
wrong_out="$(un_purgedb "99" --purge-database)"
wrong_rc=$?
nonzero "purge-database: wrong count exits non-zero"           "$wrong_rc"
want    "purge-database: wrong count reports the mismatch"     "count mismatch" "$wrong_out"
isdir   "purge-database: wrong count leaves .beads intact"     "$PURGEDB_DIR/.beads"

# Right count: database, Dolt data and test Dolt data are all removed; exit 0.
_seed_purgedb
right_out="$(un_purgedb "3" --purge-database)"
right_rc=$?
iszero "purge-database: right count exits 0"                "$right_rc"
nofile "purge-database: right count removes the database"   "$PURGEDB_DIR"
nofile "purge-database: right count removes Dolt data"       "$PURGEDB_DOLT"
nofile "purge-database: right count removes test Dolt data"  "$PURGEDB_TESTDB"

# Plain --purge (no --purge-database): never prompts for a count, never touches the DB.
_seed_purgedb
plain_out="$(un_purgedb "3" --purge)"
plain_rc=$?
iszero "purge-database: plain --purge exits 0"                     "$plain_rc"
nowant "purge-database: plain --purge does not prompt for a count" "Type the count" "$plain_out"
isdir  "purge-database: plain --purge leaves .beads intact"        "$PURGEDB_DIR/.beads"

rm -rf "$PURGEDB_DIR" "$PURGEDB_DOLT" "$PURGEDB_TESTDB"

# ==========================================================================
echo
echo "TIMER RACE (T2, sp-6k3pr) — a timer firing mid-uninstall does not resurrect its service:"
# ==========================================================================
# spira-broker.service/.timer are only in the manifest when SPIRA_BROKER_BIN is
# executable (units.sh); set it to a stub so owned.sh includes them exactly as
# a real broker install would.
RACE_UNIT_SVC="spira-broker-race.service"
RACE_UNIT_TMR="spira-broker-race.timer"
RACE_STATE="$TMP/race-state"
RACE_BIN="$TMP/race-bin"
mkdir -p "$RACE_STATE" "$RACE_BIN"
printf '#!/bin/sh\n' > "$RACE_BIN/broker"; chmod +x "$RACE_BIN/broker"

# Mock systemctl that tracks the service's running state on disk. Stopping the
# SERVICE while its TIMER has not yet been stopped simulates the timer firing
# concurrently and restarting it — the exact race sp-6k3pr describes. Stopping
# the timer first (the fix) disarms this before the service is ever touched.
cat > "$RACE_BIN/systemctl" <<MOCK
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$RACE_STATE/calls.log"
case "\$*" in
    *"stop $RACE_UNIT_TMR"*) : > "$RACE_STATE/timer_stopped" ;;
    *"stop $RACE_UNIT_SVC"*)
        rm -f "$RACE_STATE/active"
        [ -e "$RACE_STATE/timer_stopped" ] || : > "$RACE_STATE/active"
        ;;
    *"start $RACE_UNIT_SVC"*) : > "$RACE_STATE/active" ;;
    *"list-units"*) printf '' ;;
esac
exit 0
MOCK
chmod +x "$RACE_BIN/systemctl"

# POSITIVE CONTROL: prove the mock reproduces the race under the pre-fix
# order (service stopped before its timer) — a clean result below is
# meaningless unless this mock can also report a dirty one.
: > "$RACE_STATE/active"; rm -f "$RACE_STATE/timer_stopped"
"$RACE_BIN/systemctl" --user stop "$RACE_UNIT_SVC" >/dev/null
"$RACE_BIN/systemctl" --user stop "$RACE_UNIT_TMR" >/dev/null
[ -e "$RACE_STATE/active" ] \
    && ok  "race: positive control — service-before-timer order lets the firing resurrect it" \
    || bad "race: positive control — service-before-timer order lets the firing resurrect it" \
           "expected the mock to show the service resurrected"

# THE REAL RUN: uninstall.sh must stop every .timer before any .service, so
# by the time it reaches the broker service the firing can no longer happen.
: > "$RACE_STATE/active"; rm -f "$RACE_STATE/timer_stopped"
race_out="$(env -i \
    "PATH=$PATH" \
    "HOME=$TMP/home" \
    SPIRA_CONF=/nonexistent \
    "SPIRA_PATH=$RACE_BIN" \
    "SPIRA_RUN=$SPIRA_RUN_DIR" \
    "SPIRA_HOME=$FIXTURE/spira" \
    "SPIRA_PROD=$FIXTURE/spira" \
    "SPIRA_REPO=$FAKE_REPO" \
    "SPIRA_COCKPIT=$REAL_COCKPIT" \
    "SPIRA_INSTANCE=test" \
    "SPIRA_BROKER_BIN=$RACE_BIN/broker" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    "SPIRA_SYSTEMCTL=$RACE_BIN/systemctl" \
    "SPIRA_LOGINCTL=$MOCK_BIN/loginctl" \
    "SPIRA_TMUX=$MOCK_BIN/tmux" \
    bash "$FIXTURE/spira/uninstall.sh" test --yes 2>&1)"
race_rc=$?

iszero "race: uninstall.sh --yes exits 0" "$race_rc"
[ -e "$RACE_STATE/active" ] \
    && bad "race: timer-fires-mid-uninstall does not resurrect the service (T2)" \
           "service ended active — timer was not stopped before the service" \
    || ok  "race: timer-fires-mid-uninstall does not resurrect the service (T2)"

rm -rf "$RACE_STATE" "$RACE_BIN"

# ==========================================================================
echo
tl_summary
