#!/usr/bin/env bash
#
# test-doctor-goal.sh — doctor.sh fails when SPIRA_GOAL names no bead, and passes
#   when the goal bead exists.
#
#   ./test-doctor-goal.sh
#
# defect: sp-ejf3
# covers: spira/doctor.sh spira/sentinel.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

echo "test-doctor-goal.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# make_fake_bd <show-rc> <show-output>: writes a fake bd to $TMP/bin/bd.
# The fake handles the three calls doctor.sh makes that touch the database:
#   list --limit 1 --json   — "bd can read it"
#   migrate schema          — schema version check
#   show <id>               — goal-bead existence check (what we're testing)
#   anything else           — exit 0 silently
make_fake_bd() {
    local show_rc="$1" show_out="$2"
    mkdir -p "$TMP/bin"
    cat > "$TMP/bin/bd" <<FAKESCRIPT
#!/usr/bin/env bash
case "\$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"--limit"*"1"*"--json"*|*"list"*"--json"*"--limit"*"1"*)
        printf '[{"id":"sp-test"}]\n'; exit 0 ;;
    *"show"*) printf '%s\n' "$show_out"; exit $show_rc ;;
    *) exit 0 ;;
esac
FAKESCRIPT
    chmod +x "$TMP/bin/bd"
}

setup_env() {
    mkdir -p "$TMP/db/.beads" "$TMP/run" "$TMP/home"
}

run_doctor() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$TMP/bin" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_GOAL=sp-test \
        "$@" \
        bash "$HERE/doctor.sh" 2>/dev/null
}

setup_env

# ==========================================================================
echo
echo "positive control — fake bd that mimics a missing goal bead is detectable:"
# ==========================================================================
# The fake bd returns no JSON output for 'show'. Verify the doctor output
# actually names a FAIL so the absence assertion below is meaningful.
make_fake_bd 1 ""
ctrl_out="$(run_doctor || true)"
want "positive control: FAIL line appears in output" "FAIL" "$ctrl_out"
want "positive control: goal bead section appears" "goal bead" "$ctrl_out"

# ==========================================================================
echo
echo "unresolvable goal — doctor.sh fails and names SPIRA_GOAL and the remedy:"
# ==========================================================================
make_fake_bd 1 ""
run_doctor > "$TMP/missing.out" 2>/dev/null && missing_rc=0 || missing_rc=$?
missing_out="$(cat "$TMP/missing.out")"

want  "missing: FAIL line appears"          "FAIL"       "$missing_out"
want  "missing: goal id named in output"    "sp-test"    "$missing_out"
want  "missing: 'goal reached' risk named"  "goal reached" "$missing_out"
if [ "${missing_rc:-0}" -ne 0 ]; then
    ok "missing: doctor.sh exits non-zero (rc=$missing_rc)"
else
    bad "missing: doctor.sh exits non-zero" "exited 0"
fi

# ==========================================================================
echo
echo "goal exists — doctor.sh reports ok and does not fail:"
# ==========================================================================
make_fake_bd 0 '{"id":"sp-test","status":"open"}'
exists_out="$(run_doctor || true)"
want   "exists: ok line appears for goal bead"  "ok" "$(printf '%s\n' "$exists_out" | grep 'goal bead' || true)"
nowant "exists: no FAIL for goal bead"          "FAIL" "$(printf '%s\n' "$exists_out" | grep 'goal bead' || true)"

# ==========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
