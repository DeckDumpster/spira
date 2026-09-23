#!/usr/bin/env bash
#
# test-doctor-installing.sh — doctor.sh under SPIRA_DOCTOR_INSTALLING=1 downgrades to WARN
# the checks whose state install.sh's own phases create.
#
#   ./test-doctor-installing.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL. Without SPIRA_DOCTOR_INSTALLING the checks are FAIL, so the
#    WARN cases below prove something real.
# 2. INSTALLING — Dolt data dir. SPIRA_DOLT_DATA set but directory absent: WARN naming
#    phase 3, no FAIL.
# 3. INSTALLING — dolt-beads.service. Service inactive under SPIRA_DOLT_DATA: WARN naming
#    phase 4, no FAIL.
# 4. INSTALLING — home-repo row. Home repo absent from the map: WARN naming phase 1, no
#    FAIL.
# 5. EXIT CODE. With SPIRA_DOCTOR_INSTALLING=1 and all three conditions present, doctor.sh
#    exits 0 (no fatals).
# 6. INSTALLING — bd cannot read (dolt stopped). An existing database that bd cannot query
#    because dolt-beads.service is inactive: WARN naming phase 4, no FAIL.
#    Simulates the re-install after uninstall path: Phase A installs (creates db in server
#    mode), Phase A uninstalls (stops dolt-beads.service), Phase B installs (doctor sees
#    existing db but dolt is down). Without this downgrade, Phase B's preflight exits 1.
# 7. POSITIVE CONTROL for case 6. Same condition without SPIRA_DOCTOR_INSTALLING → FAIL.
#
# covers: spira/doctor.sh
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

echo "test-doctor-installing.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

BIN="$TMP/bin"
FAKE_DB="$TMP/db"
mkdir -p "$BIN" "$FAKE_DB" "$TMP/run" "$TMP/home"

# Fake bd handles the calls doctor.sh makes.
cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

# Fake systemctl: dolt-beads.service is always inactive; everything else is active.
cat > "$BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*"dolt-beads"*) exit 1 ;;
    *"is-active"*"--quiet"*) exit 0 ;;
    *"list-units"*"active"*"spira-aeon"*) true ;;
    *"list-unit-files"*"spira-watch"*) true ;;
    *"list-units"*"spira-watch"*) true ;;
    *"is-active"*) printf 'active\n' ;;
    *"is-system-running"*) printf 'running\n' ;;
    *"list-timers"*) true ;;
    *"is-enabled"*) printf 'enabled\n' ;;
esac
exit 0
MOCK
chmod +x "$BIN/systemctl"

# Repo-map: exists but has no repo rows — the home-repo row is missing by design.
printf '# repo-map — no rows\n' > "$TMP/repo-map"

# Fake cargo: reports a version >= 1.78.0 so the cargo-version check passes.
# Without this, the container's old rustc triggers an unrelated FAIL.
cat > "$BIN/cargo" <<'CARGO'
#!/usr/bin/env bash
[ "${1:-}" = "--version" ] && printf 'cargo 1.82.0 (abc123)\n' && exit 0
exit 0
CARGO
chmod +x "$BIN/cargo"

# Fake broker: the broker check FAILs when the unit is enabled but binary absent.
# Provide a stub binary so that check passes.
cat > "$BIN/broker" <<'BROKER'
#!/usr/bin/env bash
exit 0
BROKER
chmod +x "$BIN/broker"

touch "$TMP/watchers-empty"

# Doctor checks git -C $SPIRA_REPO config core.hooksPath; the container mounts the
# worktree but not the main git dir, so git can't follow the gitdir pointer and always
# returns empty. Fake git returns "spira/hooks" for that read; passes everything else to
# the real git (stripping $BIN from PATH first to avoid calling itself).
REAL_GIT="$(PATH="/usr/local/bin:/usr/bin:/bin" command -v git || echo /usr/bin/git)"
cat > "$BIN/git" <<FAKEGIT
#!/usr/bin/env bash
for arg; do [ "\$arg" = "core.hooksPath" ] && { printf 'spira/hooks\n'; exit 0; }; done
exec $REAL_GIT "\$@"
FAKEGIT
chmod +x "$BIN/git"

# SPIRA_DOLT_DATA points at a directory that does not yet exist.
DOLT_DIR="$TMP/dolt-data-not-created-yet"

run_doctor() {
    local extra_env=()
    [ "${1:-}" = "--installing" ] && extra_env=(SPIRA_DOCTOR_INSTALLING=1)
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_DB="$FAKE_DB" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP="$TMP/repo-map" \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        SPIRA_DOLT_DATA="$DOLT_DIR" \
        SPIRA_HOME_REPO=test-home-repo \
        SPIRA_OPERATED=0 \
        SPIRA_BROKER_BIN="$BIN/broker" \
        "${extra_env[@]+"${extra_env[@]}"}" \
        bash "$HERE/doctor.sh" 2>/dev/null
}

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — three checks are FAIL without SPIRA_DOCTOR_INSTALLING:"
# ==========================================================================
ctrl_out="$(run_doctor || true)"

want "positive control: dolt-dir FAIL" "FAIL" \
    "$(printf '%s\n' "$ctrl_out" | grep 'SPIRA_DOLT_DATA.*does not exist' || true)"
want "positive control: service FAIL" "FAIL" \
    "$(printf '%s\n' "$ctrl_out" | grep 'dolt-beads.service is not active' || true)"
want "positive control: home-repo FAIL" "FAIL" \
    "$(printf '%s\n' "$ctrl_out" | grep 'has no row in the map' || true)"

# ==========================================================================
echo
echo "2-4. INSTALLING CASE — all three are WARN naming the creating phase, no FAIL:"
# ==========================================================================
inst_out="$(run_doctor --installing || true)"

want   "installing: dolt-dir WARN names phase 3" "phase 3" \
    "$(printf '%s\n' "$inst_out" | grep 'SPIRA_DOLT_DATA.*does not exist' || true)"
nowant "installing: dolt-dir no FAIL" "FAIL" \
    "$(printf '%s\n' "$inst_out" | grep 'SPIRA_DOLT_DATA.*does not exist' || true)"

want   "installing: service WARN names phase 4" "phase 4" \
    "$(printf '%s\n' "$inst_out" | grep 'dolt-beads.service' || true)"
nowant "installing: service no FAIL" "FAIL" \
    "$(printf '%s\n' "$inst_out" | grep 'dolt-beads.service' || true)"

want   "installing: home-repo WARN names phase 1" "phase 1" \
    "$(printf '%s\n' "$inst_out" | grep 'has no row in the map' || true)"
nowant "installing: home-repo no FAIL" "FAIL" \
    "$(printf '%s\n' "$inst_out" | grep 'has no row in the map' || true)"

# ==========================================================================
echo
echo "5. EXIT CODE — doctor exits 0 with SPIRA_DOCTOR_INSTALLING=1:"
# ==========================================================================
if run_doctor --installing >/dev/null 2>&1; then
    ok "installing: doctor exits 0"
else
    bad "installing: doctor exits 0" "non-zero exit"
fi

# ==========================================================================
echo
echo "6-7. INSTALLING — bd cannot read (dolt stopped after prior uninstall):"
# ==========================================================================
# Fixture: existing .beads directory (db was created by a prior install), but
# bd fails to connect because dolt-beads.service was removed by uninstall.sh.
# doctor.sh calls `bd` by name, so put the failing stub first on PATH via
# a separate SPIRA_PATH directory; other tools (systemctl, git, etc.) still
# resolve from $BIN.
FAKE_DB_EXISTS="$TMP/db-exists"
BIN_FAIL="$TMP/bin-fail"
mkdir -p "$FAKE_DB_EXISTS/.beads" "$BIN_FAIL"

cat > "$BIN_FAIL/bd" <<'FAKEFAIL'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*)
        printf 'connection refused: dolt server not running\n' >&2
        exit 1 ;;
    *) exit 0 ;;
esac
FAKEFAIL
chmod +x "$BIN_FAIL/bd"

run_doctor_db_exists() {
    local extra_env=()
    [ "${1:-}" = "--installing" ] && extra_env=(SPIRA_DOCTOR_INSTALLING=1)
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN_FAIL:$BIN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_DB="$FAKE_DB_EXISTS" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP="$TMP/repo-map" \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        SPIRA_DOLT_DATA="$DOLT_DIR" \
        SPIRA_HOME_REPO=test-home-repo \
        SPIRA_OPERATED=0 \
        SPIRA_BROKER_BIN="$BIN/broker" \
        "${extra_env[@]+"${extra_env[@]}"}" \
        bash "$HERE/doctor.sh" 2>/dev/null
}

inst_db_out="$(run_doctor_db_exists --installing || true)"
want   "installing: bd-cannot-read WARN names phase 4" "phase 4" \
    "$(printf '%s\n' "$inst_db_out" | grep 'bd cannot read' || true)"
nowant "installing: bd-cannot-read no FAIL" "FAIL" \
    "$(printf '%s\n' "$inst_db_out" | grep 'bd cannot read' || true)"

ctrl_db_out="$(run_doctor_db_exists || true)"
want "positive control: bd-cannot-read FAIL without installing flag" "FAIL" \
    "$(printf '%s\n' "$ctrl_db_out" | grep 'bd cannot read' || true)"

if run_doctor_db_exists --installing >/dev/null 2>&1; then
    ok "installing: doctor exits 0 with existing db and dolt stopped"
else
    bad "installing: doctor exits 0 with existing db and dolt stopped" "non-zero exit"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
