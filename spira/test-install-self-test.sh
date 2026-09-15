#!/usr/bin/env bash
#
# test-install-self-test.sh — SPIRA_SELF_TEST gates the suites timer.
#
#   ./test-install-self-test.sh
#
# WHAT THIS SUITE PROVES
# ----------------------
# SPIRA_SELF_TEST=0 is the right default for consumer installations (from a release
# tarball with no .git directory). Filing self-test beads into the operator's work
# queue is the defect: sp-uqod.
#
# THREE PROPERTIES are verified, all three required for the fix to hold:
#
#   A  POSITIVE CONTROL: with SPIRA_SELF_TEST=1, install.sh --render includes the
#      suites timer unit. Without this, the silence in case B could mean the timer
#      was never renderable to begin with (law-absence-needs-a-positive-control).
#
#   B  CONSUMER: with SPIRA_SELF_TEST=0, install.sh --render produces no suites
#      timer unit. An operator on a consumer installation must not see the timer
#      installed by install.sh.
#
#   C  AUTO-DETECT: conf.sh derives SPIRA_SELF_TEST from SPIRA_REPO_DERIVED (the
#      git root of SPIRA_HOME). A SPIRA_HOME outside any git checkout gets
#      SPIRA_SELF_TEST=0; SPIRA_HOME inside a git checkout gets SPIRA_SELF_TEST=1.
#      An explicit SPIRA_SELF_TEST=0 in spira.conf overrides the detection.
#
# defect: sp-uqod
# covers: systemd/units.sh spira/conf.sh systemd/install.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
REAL_COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-install-self-test.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture: a minimal harness tree that install.sh --render can run against.
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
mkdir -p "$FIXTURE/systemd" "$FIXTURE/spira"

for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer; do
    [ -e "$f" ] || continue
    ln -s "$f" "$FIXTURE/systemd/$(basename "$f")"
done
ln -s "$HERE/../systemd/install.sh" "$FIXTURE/systemd/install.sh"
for f in conf.sh watchd.sh lib.sh; do
    [ -e "$HERE/$f" ] && ln -s "$HERE/$f" "$FIXTURE/spira/$f"
done

printf '# empty — test fixture\n' > "$FIXTURE/spira/watchers"
printf '# empty\n' > "$FIXTURE/spira/repo-map.example"
printf '#!/usr/bin/env bash\nexit 0\n' > "$FIXTURE/spira/install-session-hook.sh"
chmod +x "$FIXTURE/spira/install-session-hook.sh"

DEST="$TMP/home/.config/systemd/user"
SPIRA_RUN_DIR="$TMP/run"
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$DEST" "$SPIRA_RUN_DIR" "$MOCK_BIN"

cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *is-active*)  printf 'active\n' ;;
    *is-enabled*) printf 'enabled\n' ;;
    *"list-unit-files"*|*"list-units"*|*"list-timers"*) : ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/loginctl"
chmod +x "$MOCK_BIN/loginctl"

# render <SPIRA_SELF_TEST_value>: run install.sh --render with the given value.
render() {
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_WATCHERS=$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA= \
        SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_PROD=$HERE" \
        "SPIRA_REPO=$REAL_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "SPIRA_SELF_TEST=$1" \
        SPIRA_INSTALL_FORCE=1 \
        bash "$FIXTURE/systemd/install.sh" --render 2>&1
}

# ==========================================================================
echo
echo "A: POSITIVE CONTROL — SPIRA_SELF_TEST=1 must render the suites timer:"
# ==========================================================================
pos_out="$(render 1)"; pos_rc=$?
is     "A: render exits 0 with SPIRA_SELF_TEST=1"                    "0"    "$pos_rc"
want   "A: suites service in rendered units"  "spira-suites-prod.service" "$pos_out"
want   "A: suites timer in rendered units"    "spira-suites-prod.timer"   "$pos_out"

# ==========================================================================
echo
echo "B: CONSUMER — SPIRA_SELF_TEST=0 must produce no suites timer unit:"
# ==========================================================================
con_out="$(render 0)"; con_rc=$?
is     "B: render exits 0 with SPIRA_SELF_TEST=0"                    "0"    "$con_rc"
nowant "B: suites service absent when SPIRA_SELF_TEST=0" "spira-suites-prod.service" "$con_out"
nowant "B: suites timer absent when SPIRA_SELF_TEST=0"   "spira-suites-prod.timer"   "$con_out"

# ==========================================================================
echo
echo "C: AUTO-DETECT — conf.sh derives SPIRA_SELF_TEST from .git presence:"
# ==========================================================================
# The detection uses SPIRA_REPO_DERIVED, which is computed from
# `git -C "$SPIRA_HOME" rev-parse --show-toplevel`. To test both outcomes,
# we need SPIRA_HOME to point at (a) a non-git directory and (b) a git checkout.
#
# For (a), we create a stub harness under $TMP, which is /tmp — outside any
# git checkout in the testenv container. git will walk upward from there and
# find nothing, leaving SPIRA_REPO_DERIVED empty (fallback to $TMP's parent),
# which has no .git, so SPIRA_SELF_TEST defaults to 0.
NO_GIT="$TMP/nogit"
mkdir -p "$NO_GIT/spira"
cp "$HERE/conf.sh" "$NO_GIT/spira/conf.sh"

detect() {      # detect <spira-home-dir> -> SPIRA_SELF_TEST value or UNSET
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/detect-home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_RUN=$TMP/detect-run" \
        "SPIRA_HOME=$1" \
        bash -c '. "$SPIRA_HOME/conf.sh" 2>/dev/null
                 printf "%s" "${SPIRA_SELF_TEST:-UNSET}"'
}
mkdir -p "$TMP/detect-home" "$TMP/detect-run"

# C1: SPIRA_HOME points into /tmp (no git checkout there) → SPIRA_SELF_TEST=0.
nogit_val="$(detect "$NO_GIT/spira")"
is "C1: no .git above SPIRA_HOME → SPIRA_SELF_TEST=0" "0" "$nogit_val"

# C2: SPIRA_HOME points at the real installed harness (inside a git checkout) → 1.
git_val="$(detect "$HERE")"
is "C2: .git present above SPIRA_HOME → SPIRA_SELF_TEST=1" "1" "$git_val"

# C3: SPIRA_SELF_TEST=0 in spira.conf overrides detection even on a git checkout.
OVERRIDE_CONF="$TMP/override.conf"
printf 'SPIRA_SELF_TEST=0\n' > "$OVERRIDE_CONF"
override_val="$(
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/detect-home" \
        "SPIRA_CONF=$OVERRIDE_CONF" \
        "SPIRA_RUN=$TMP/detect-run" \
        "SPIRA_HOME=$HERE" \
        bash -c '. "$SPIRA_HOME/conf.sh" 2>/dev/null
                 printf "%s" "${SPIRA_SELF_TEST:-UNSET}"'
)"
is "C3: SPIRA_SELF_TEST=0 in spira.conf overrides .git detection" "0" "$override_val"

# ==========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
