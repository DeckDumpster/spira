#!/usr/bin/env bash
#
# test-doctor-gh-intake-skip.sh — doctor.sh FAILs when the gh-intake service has
# been persistently skipped by its ExecCondition while SPIRA_GH_INTAKE_REPO is set.
#
# WHAT THIS TESTS
# ---------------
# A timer-backed service whose ExecCondition tests a shell variable that systemd
# cannot read (e.g. from conf.sh) will skip on every firing and look healthy from
# every systemd state query. The only visible symptom is the journal. doctor.sh
# must detect the pattern and FAIL when:
#   1. SPIRA_GH_INTAKE_REPO is non-empty (intake is configured to run), AND
#   2. the service's recent journal entries show exec-condition skips with no
#      successful runs.
#
# POSITIVE CONTROL. The FAIL case is tested before the clean case. A probe that
# only passes a clean journal would be identical to one that never checks.
#
# covers: spira/doctor.sh spira/gh-intake.sh systemd/spira-gh-intake.service
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-doctor-gh-intake-skip.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

BIN="$TMP/bin"
mkdir -p "$BIN" "$TMP/db/.beads" "$TMP/run"
touch "$TMP/watchers-empty"

cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

FAKE_HOME="$TMP/home"
mkdir -p "$FAKE_HOME/.config/systemd/user"

cat > "$BIN/sc" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)  printf 'active\n' ;;
    *"is-enabled"*) printf 'enabled\n' ;;
    *) exit 0 ;;
esac
MOCK
chmod +x "$BIN/sc"

# jctl-skip: all recent entries are exec-condition skips, no successful runs.
cat > "$BIN/jctl-skip" <<'MOCK'
#!/usr/bin/env bash
printf 'Sep 20 15:09 systemd[1]: spira-gh-intake-prod.service: Skipped due to '\''exec-condition'\''\n'
printf 'Sep 20 14:54 systemd[1]: spira-gh-intake-prod.service: Skipped due to '\''exec-condition'\''\n'
printf 'Sep 20 14:39 systemd[1]: spira-gh-intake-prod.service: Skipped due to '\''exec-condition'\''\n'
MOCK
chmod +x "$BIN/jctl-skip"

# jctl-clean: recent entries show successful runs.
cat > "$BIN/jctl-clean" <<'MOCK'
#!/usr/bin/env bash
printf 'Sep 20 15:09 systemd[1]: spira-gh-intake-prod.service: Succeeded.\n'
printf 'Sep 20 14:54 systemd[1]: spira-gh-intake-prod.service: Finished Spira GitHub issue intake.\n'
MOCK
chmod +x "$BIN/jctl-clean"

# jctl-empty: no journal entries (unit installed but never fired).
cat > "$BIN/jctl-empty" <<'MOCK'
#!/usr/bin/env bash
exit 0
MOCK
chmod +x "$BIN/jctl-empty"

run_doctor() {
    local jctl="$1" repo="${2:-}"
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/sc" \
        SPIRA_JOURNALCTL="$jctl" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        SPIRA_GH_INTAKE_REPO="$repo" \
        bash "$HERE/doctor.sh" 2>/dev/null || true
}

# ===========================================================================
echo
echo "positive control — skip with repo set produces FAIL before clean case trusted:"
# ===========================================================================
skip_out="$(run_doctor "$BIN/jctl-skip" "fixture-owner/fixture-repo")"
want "positive control: FAIL line present" \
    "  FAIL  spira-gh-intake-prod.service" "$skip_out"
want "positive control: names the cause" "exec-condition" "$skip_out"
want "positive control: names the remedy" "install.sh" "$skip_out"

# ===========================================================================
echo
echo "clean case — successful runs: no FAIL:"
# ===========================================================================
clean_out="$(run_doctor "$BIN/jctl-clean" "fixture-owner/fixture-repo")"
want   "clean: OK line present" "github intake — no persistent exec-condition skip" "$clean_out"
nowant "clean: no FAIL" "  FAIL  spira-gh-intake" "$clean_out"

# ===========================================================================
echo
echo "empty journal — unit never fired: no FAIL (not yet evidence of a problem):"
# ===========================================================================
empty_out="$(run_doctor "$BIN/jctl-empty" "fixture-owner/fixture-repo")"
nowant "empty journal: no FAIL" "  FAIL  spira-gh-intake" "$empty_out"

# ===========================================================================
echo
echo "repo not configured — section says off, no FAIL:"
# ===========================================================================
noconf_out="$(run_doctor "$BIN/jctl-skip" "")"
nowant "not configured: no FAIL" "  FAIL  spira-gh-intake" "$noconf_out"
want   "not configured: info line" "intake is off" "$noconf_out"

# ===========================================================================
echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
