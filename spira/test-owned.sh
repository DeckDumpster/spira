#!/usr/bin/env bash
#
# test-owned.sh — owned.sh declares and checks every artifact kind an installation owns.
#
#   ./test-owned.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. KINDS PRESENT: owned.sh list produces at least one row for every kind: unit, linger,
#    runtime-tree, database, session-hook, binary, dolt-yaml, cockpit-pane, alert-dropin.
# 2. UNIT NAMES MATCH: owned.sh list unit rows match the unit names systemd/install.sh --render
#    would install, for two different instance names (prod and test).
# 3. CHECK ABSENT: with an empty UNITDIR, check reports 'absent' for all unit rows.
# 4. CHECK PRESENT: after rendering units to UNITDIR, check reports 'present' for all unit rows.
# 5. CHECK DRIFTED: after modifying one installed unit, check reports 'drifted' for that row.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): 'present' for matching units is
# confirmed before 'drifted' is tested; a check that always reports 'absent' would fail this.
#
# HERMETIC: fake HOME (mktemp), SPIRA_SYSTEMCTL/SPIRA_LOGINCTL/SPIRA_TMUX stubs, no real
# config file, no production database — law-gates-run-in-a-clean-environment.
#
# covers: spira/owned.sh systemd/units.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
REAL_COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
nonzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }

echo "test-owned.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture: minimal harness tree, enough to run owned.sh and install.sh --render/--diff.
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
mkdir -p "$FIXTURE/systemd" "$FIXTURE/spira"

for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer; do
    [ -e "$f" ] || continue
    ln -s "$f" "$FIXTURE/systemd/$(basename "$f")"
done
for f in install.sh units.sh; do
    [ -e "$HERE/../systemd/$f" ] && ln -s "$HERE/../systemd/$f" "$FIXTURE/systemd/$f"
done
for f in conf.sh watchd.sh lib.sh install-session-hook.sh owned.sh suite-covers.sh; do
    [ -e "$HERE/$f" ] && ln -s "$HERE/$f" "$FIXTURE/spira/$f"
done
printf '# empty — test fixture\n' > "$FIXTURE/spira/watchers"
printf '# empty\n'                 > "$FIXTURE/spira/repo-map.example"

FAKE_HOME="$TMP/home"
UNITDIR="$FAKE_HOME/.config/systemd/user"
RUN_DIR="$TMP/run"
DB_DIR="$TMP/db"
MOCK_BIN="$TMP/mock-bin"
MOCK_LOG="$TMP/mock-systemctl.log"
TESTDB_DIR="$TMP/testdb-data"
mkdir -p "$UNITDIR" "$RUN_DIR" "$MOCK_BIN" "$TESTDB_DIR"

# Stub systemctl: owned.sh passes SPIRA_SYSTEMCTL to install.sh --diff, which uses
# it for the live-aeon check (skipped by SPIRA_INSTALL_FORCE, but the stub keeps
# install.sh's list-unit-files calls safe).
cat > "$MOCK_BIN/systemctl" <<MOCK
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "\${MOCK_LOG:-/dev/null}"
case "\$*" in
    *is-active*)         printf 'active\n' ;;
    *list-unit-files*)   true ;;
    *list-units*)        true ;;
    *list-timers*)       true ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"

# Stub loginctl: linger check in owned.sh uses SPIRA_LOGINCTL.
cat > "$MOCK_BIN/loginctl" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *show-user*Linger*) printf 'Linger=yes\n'; exit 0 ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/loginctl"

# Stub tmux: cockpit-pane check uses SPIRA_TMUX. No panes in test — exits 1.
cat > "$MOCK_BIN/tmux" <<'MOCK'
#!/usr/bin/env bash
exit 1
MOCK
chmod +x "$MOCK_BIN/tmux"

# Stub dolt: install.sh resolves DOLT via `command -v dolt` to decide whether
# @DOLT@ can be substituted in unit templates. Without a stub, render() exits 1
# for dolt-beads*.service on any box without dolt installed, causing --render to
# exit early (missing later units) and --diff to skip those units (reporting them
# as present instead of absent).
cat > "$MOCK_BIN/dolt" <<'MOCK'
#!/usr/bin/env bash
exit 0
MOCK
chmod +x "$MOCK_BIN/dolt"

# A fake alert template in UNITDIR so alert-dropin rows appear.
touch "$UNITDIR/alert-prod@.service"
ALERT_GLOB="alert-prod@.service"

# Fake loom, broker, and panel paths so binary rows appear and units.sh includes
# loom/broker units (which it gates on SPIRA_LOOM_BIN/SPIRA_BROKER_BIN being executable).
FAKE_LOOM="$TMP/loom"
FAKE_BROKER="$TMP/broker"
FAKE_PANEL="$TMP/panel"
touch "$FAKE_LOOM" "$FAKE_BROKER" "$FAKE_PANEL"
chmod +x "$FAKE_LOOM" "$FAKE_BROKER" "$FAKE_PANEL"

# A fake dolt data directory so dolt-yaml rows appear.
DOLT_DIR="$TMP/dolt-data"
mkdir -p "$DOLT_DIR"

# ---------------------------------------------------------------------------
# run_owned [instance] [subcommand] — run owned.sh in the controlled environment.
# ---------------------------------------------------------------------------
run_owned() {
    local inst="${1:-}" subcmd="${2:-list}"
    MOCK_LOG="$MOCK_LOG" \
    env -i \
        "PATH=$MOCK_BIN:$PATH" \
        "HOME=$FAKE_HOME" \
        "USER=testuser" \
        "SPIRA_CONF=/nonexistent" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_REPO_MAP=$FIXTURE/spira/repo-map.example" \
        "SPIRA_WATCHERS=$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA="$DOLT_DIR" \
        "SPIRA_TESTDB_DATA=$TESTDB_DIR" \
        "SPIRA_ALERT_GLOB=$ALERT_GLOB" \
        "SPIRA_LOOM_BIN=$FAKE_LOOM" \
        "SPIRA_BROKER_BIN=$FAKE_BROKER" \
        "SPIRA_PANEL=$FAKE_PANEL" \
        "SPIRA_RUN=$RUN_DIR" \
        "SPIRA_DB=$DB_DIR" \
        "SPIRA_CLIENT_SETTINGS=$FAKE_HOME/.claude/settings.json" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "SPIRA_SYSTEMCTL=$MOCK_BIN/systemctl" \
        "SPIRA_LOGINCTL=$MOCK_BIN/loginctl" \
        "SPIRA_TMUX=$MOCK_BIN/tmux" \
        "SPIRA_INSTALL_FORCE=1" \
        "MOCK_LOG=$MOCK_LOG" \
        bash "$FIXTURE/spira/owned.sh" "$subcmd" ${inst:+"$inst"} 2>/dev/null
}

# render_units <instance> — unit names from install.sh --render, sorted.
# Uses the same SPIRA_DOLT_DATA as run_owned so the unit sets agree.
render_units() {
    local inst="$1"
    env -i \
        "PATH=$MOCK_BIN:$PATH" \
        "HOME=$FAKE_HOME" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_CONF=/nonexistent" \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_REPO_MAP=$FIXTURE/spira/repo-map.example" \
        "SPIRA_WATCHERS=$FIXTURE/spira/watchers" \
        "SPIRA_DOLT_DATA=$DOLT_DIR" \
        "SPIRA_TESTDB_DATA=$TESTDB_DIR" \
        "SPIRA_LOOM_BIN=$FAKE_LOOM" \
        "SPIRA_BROKER_BIN=$FAKE_BROKER" \
        "SPIRA_INSTALL_FORCE=1" \
        bash "$FIXTURE/systemd/install.sh" "$inst" --render 2>/dev/null \
    | awk '/^===== /{gsub(/^===== /,""); gsub(/ =====$/, ""); print}' \
    | sort
}

# ==========================================================================
echo
echo "1. LIST — all required kinds present:"
# ==========================================================================
list_out="$(run_owned "" list)"; list_rc=$?
iszero "list exits 0" "$list_rc"

for kind in unit linger runtime-tree database session-hook binary dolt-yaml cockpit-pane alert-dropin; do
    if printf '%s\n' "$list_out" | grep -q "^${kind}|"; then
        ok "list: kind '$kind' present"
    else
        bad "list: kind '$kind'" "missing from list output"
    fi
done

# Row format sanity: each row has exactly 5 pipe-separated fields.
malformed="$(printf '%s\n' "$list_out" | awk -F'|' 'NF != 5 {print NR": "$0}')"
[ -z "$malformed" ] && ok "list: all rows have 5 fields" \
    || bad "list: malformed rows" "$malformed"

# ==========================================================================
echo
echo "2. UNIT ROWS MATCH install.sh --render (prod and test instances):"
# ==========================================================================
for inst in prod test; do
    owned_units="$(run_owned "$inst" list 2>/dev/null \
        | awk -F'|' '$1=="unit"{print $2}' | sort)"
    render_out="$(render_units "$inst")"
    if [ "$owned_units" = "$render_out" ]; then
        ok "unit rows match install.sh --render for instance '$inst'"
    else
        bad "unit rows for instance '$inst'" \
            "owned=$(printf '%s' "$owned_units" | wc -l | tr -d ' ') render=$(printf '%s' "$render_out" | wc -l | tr -d ' ')"
        diff <(printf '%s\n' "$owned_units") <(printf '%s\n' "$render_out") >&2 || true
    fi
done

# ==========================================================================
echo
echo "3. CHECK ABSENT — units absent when UNITDIR is empty:"
# ==========================================================================
# Remove any previously rendered units from the fixture setup.
rm -f "$UNITDIR"/*.service "$UNITDIR"/*.timer 2>/dev/null || true
touch "$UNITDIR/alert-prod@.service"  # restore the alert template

check_out="$(run_owned "" check)"; check_rc=$?
iszero "check exits 0" "$check_rc"

unit_statuses="$(printf '%s\n' "$check_out" | awk -F'|' '$1=="unit"{print $4}' | sort -u)"
if [ "$unit_statuses" = "absent" ]; then
    ok "check: all units absent in empty UNITDIR"
else
    bad "check: units with unexpected status" "expected only 'absent', got: [$unit_statuses]"
fi

# ==========================================================================
echo
echo "4. CHECK PRESENT — units present after rendering (positive control):"
# ==========================================================================
rendered="$(
    env -i \
        "PATH=$MOCK_BIN:$PATH" \
        "HOME=$FAKE_HOME" \
        "SPIRA_CONF=/nonexistent" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_REPO_MAP=$FIXTURE/spira/repo-map.example" \
        "SPIRA_WATCHERS=$FIXTURE/spira/watchers" \
        "SPIRA_DOLT_DATA=$DOLT_DIR" \
        "SPIRA_TESTDB_DATA=$TESTDB_DIR" \
        "SPIRA_LOOM_BIN=$FAKE_LOOM" \
        "SPIRA_BROKER_BIN=$FAKE_BROKER" \
        "SPIRA_RUN=$RUN_DIR" \
        "SPIRA_DB=$DB_DIR" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "SPIRA_INSTALL_FORCE=1" \
        bash "$FIXTURE/systemd/install.sh" prod --render 2>/dev/null
)"
current_unit=""
while IFS= read -r line; do
    if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
        current_unit="${BASH_REMATCH[1]}"
        > "$UNITDIR/$current_unit"
    elif [ -n "$current_unit" ]; then
        printf '%s\n' "$line" >> "$UNITDIR/$current_unit"
    fi
done <<< "$rendered"

check_after_render="$(run_owned "" check)"
present_statuses="$(printf '%s\n' "$check_after_render" | awk -F'|' '$1=="unit"{print $4}' | sort -u)"
if [ "$present_statuses" = "present" ]; then
    ok "check: all units present after rendering"
else
    bad "check: units with unexpected status after rendering" \
        "expected only 'present', got: [$present_statuses]"
fi

# ==========================================================================
echo
echo "5. CHECK DRIFTED — drifted detected after modifying a unit:"
# ==========================================================================
stale="$(find "$UNITDIR" -maxdepth 1 -name 'spira-*.service' -type f | head -1)"
if [ -n "$stale" ]; then
    printf '\n# modified by test fixture\n' >> "$stale"
    check_drifted="$(run_owned "" check)"
    drifted_count="$(printf '%s\n' "$check_drifted" | awk -F'|' '$1=="unit" && $4=="drifted"{c++} END{print c+0}')"
    if [ "$drifted_count" -ge 1 ]; then
        ok "check: drifted detected ($drifted_count row(s)) after modification"
    else
        bad "check: drifted not detected" "expected >= 1 drifted unit, got 0"
    fi
    # The unmodified units must still report present, not drifted.
    present_after="$(printf '%s\n' "$check_drifted" | awk -F'|' '$1=="unit" && $4=="present"{c++} END{print c+0}')"
    [ "$present_after" -gt 0 ] \
        && ok "check: non-drifted units still report present" \
        || bad "check: no present units after partial drift" "expected some present"
else
    bad "check drifted" "no spira-*.service file found in UNITDIR to modify"
fi

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
