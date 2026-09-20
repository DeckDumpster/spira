#!/usr/bin/env bash
#
# test-doctor-hooks-path.sh — doctor.sh detects when core.hooksPath is missing,
#   displaced, or points at a non-existent directory; SPIRA_DOCTOR_INSTALLING=1
#   downgrades the unset case to WARN; exclude.sh install arms core.hooksPath.
#
#   ./test-doctor-hooks-path.sh
#
# covers: spira/doctor.sh spira/exclude.sh install.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-doctor-hooks-path.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A fake harness repo: git repo with the harness at spira/ so SPIRA_HOME=<repo>/spira.
REPO="$TMP/repo"
git init -q "$REPO"
mkdir -p "$REPO/spira/hooks" "$TMP/db/.beads" "$TMP/run" "$TMP/home" "$TMP/bin"
touch "$REPO/spira/boundary" "$REPO/spira/gate.sh" "$REPO/spira/lib.sh"
printf '#!/usr/bin/env bash\n' > "$REPO/spira/hooks/pre-commit"
chmod +x "$REPO/spira/hooks/pre-commit"

# Minimal fake bd so other doctor sections pass without touching a real store.
cat > "$TMP/bin/bd" <<'FAKEBD'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--json"*)  printf '[{"id":"sp-test"}]\n'; exit 0 ;;
    *"show"*)           printf '[{"id":"sp-test"}]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKEBD
chmod +x "$TMP/bin/bd"

cat > "$TMP/bin/systemctl" <<'SC'
#!/usr/bin/env bash
printf 'enabled\n'; exit 0
SC
chmod +x "$TMP/bin/systemctl"

run_doctor() {
    env -i \
        PATH="$TMP/bin:/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$REPO/spira" \
        SPIRA_REPO="$REPO" \
        SPIRA_PATH="$TMP/bin" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_BD="$TMP/bin/bd" \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_GOAL=sp-test \
        SPIRA_SYSTEMCTL="$TMP/bin/systemctl" \
        "$@" \
        bash "$HERE/doctor.sh" 2>/dev/null || true
}

hooks_lines() { printf '%s\n' "$1" | grep 'core.hooksPath'; }

# ==========================================================================
echo
echo "positive control — displaced hooks path fires a FAIL:"
# ==========================================================================
# Prove the check can speak before trusting any silence from it.
git -C "$REPO" config core.hooksPath "other/hooks"
ctrl_out="$(run_doctor)"
want "displaced: FAIL fires"             "FAIL" "$ctrl_out"
want "displaced: hooks section present"  "hooks" "$ctrl_out"
want "displaced: current path named"     "other/hooks"  "$(hooks_lines "$ctrl_out")"
want "displaced: expected path named"    "spira/hooks"  "$(hooks_lines "$ctrl_out")"

# ==========================================================================
echo
echo "core.hooksPath not set — FAIL fires and names the remedy:"
# ==========================================================================
git -C "$REPO" config --unset core.hooksPath 2>/dev/null || true
unset_out="$(run_doctor)"
want "unset: FAIL fires"         "FAIL" "$unset_out"
want "unset: not set named"      "not set" "$unset_out"
want "unset: remedy names exclude.sh" "exclude.sh" "$unset_out"

# ==========================================================================
echo
echo "core.hooksPath points at nonexistent directory — FAIL fires:"
# ==========================================================================
# Correct path value, but the directory itself is gone.
git -C "$REPO" config core.hooksPath "spira/hooks"
rm -rf "$REPO/spira/hooks"
missing_out="$(run_doctor)"
want "missing dir: FAIL fires"       "FAIL"           "$(hooks_lines "$missing_out")"
want "missing dir: path named"       "spira/hooks"    "$(hooks_lines "$missing_out")"
want "missing dir: 'does not exist'" "does not exist" "$(hooks_lines "$missing_out")"
# Restore hooks directory for the passing case below.
mkdir -p "$REPO/spira/hooks"
printf '#!/usr/bin/env bash\n' > "$REPO/spira/hooks/pre-commit"
chmod +x "$REPO/spira/hooks/pre-commit"

# ==========================================================================
echo
echo "SPIRA_DOCTOR_INSTALLING=1, hooksPath unset — WARN (not FAIL):"
# ==========================================================================
# Under installing mode an unset hooksPath is expected; install.sh arms it
# in phase 5 after this check runs.
git -C "$REPO" config --unset core.hooksPath 2>/dev/null || true
ins_unset_out="$(run_doctor SPIRA_DOCTOR_INSTALLING=1)"
want   "installing unset: WARN fires"   "WARN" "$ins_unset_out"
nowant "installing unset: no FAIL"      "FAIL" "$(printf '%s\n' "$ins_unset_out" | grep 'hooksPath\|not set')"
want   "installing unset: phase 5 hint" "phase 5" "$ins_unset_out"

# ==========================================================================
echo
echo "SPIRA_DOCTOR_INSTALLING=1, hooksPath wrong path — FAIL still fires:"
# ==========================================================================
git -C "$REPO" config core.hooksPath "other/hooks"
ins_displaced_out="$(run_doctor SPIRA_DOCTOR_INSTALLING=1)"
want "installing displaced: FAIL fires" "FAIL" "$ins_displaced_out"
want "installing displaced: wrong path named" "other/hooks" "$(hooks_lines "$ins_displaced_out")"

# Restore correct path for subsequent tests.
git -C "$REPO" config core.hooksPath "spira/hooks"

# ==========================================================================
echo
echo "correct core.hooksPath — OK and no FAIL for hooks:"
# ==========================================================================
git -C "$REPO" config core.hooksPath "spira/hooks"
good_out="$(run_doctor)"
want   "correct: ok line present"   "ok" \
       "$(printf '%s\n' "$good_out" | grep 'core.hooksPath')"
nowant "correct: no FAIL for hooks" "FAIL" \
       "$(printf '%s\n' "$good_out" | grep 'core.hooksPath')"

# ==========================================================================
echo
echo "exclude.sh install on a fresh clone — arms core.hooksPath:"
# ==========================================================================
# This mirrors what install.sh phase 5 now does.  A fresh repo has no
# core.hooksPath; exclude.sh install sets it to the relative hooks dir.
FRESH="$TMP/fresh"
git init -q "$FRESH"
mkdir -p "$FRESH/spira/hooks"
printf '#!/usr/bin/env bash\n' > "$FRESH/spira/hooks/pre-commit"
chmod +x "$FRESH/spira/hooks/pre-commit"
# Positive control — confirm hooksPath absent before calling install.
before="$(git -C "$FRESH" config core.hooksPath 2>/dev/null || true)"
[ -z "$before" ] \
    && ok "fresh clone: core.hooksPath absent before install" \
    || bad "fresh clone: core.hooksPath unexpectedly set before install" "got: $before"

bash "$HERE/exclude.sh" install "$FRESH" >/dev/null 2>&1
install_rc=$?
[ "$install_rc" = 0 ] \
    && ok "fresh clone: exclude.sh install exits 0" \
    || bad "fresh clone: exclude.sh install exited $install_rc"

after="$(git -C "$FRESH" config core.hooksPath 2>/dev/null || true)"
[ "$after" = "spira/hooks" ] \
    && ok "fresh clone: core.hooksPath set to spira/hooks after install" \
    || bad "fresh clone: core.hooksPath after install" "got: [$after], want: [spira/hooks]"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
