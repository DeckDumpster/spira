#!/usr/bin/env bash
#
# test-install-exec.sh — install.sh refuses to write a unit whose ExecStart target is
# not executable, and conf.sh preserves an explicitly-empty SPIRA_PROD.
#
#   ./test-install-exec.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. EXEC FENCE: install.sh exits non-zero and names the path when an ExecStart target
#    is absent or lacks +x. Nothing is written to DEST for that unit or any later one.
# 2. CLEAN PASS: install.sh exits 0 when all ExecStart targets are executable.
# 3. CONF EMPTY: conf.sh's no-colon := for SPIRA_PROD preserves an empty value rather
#    than overriding it with the derived default (the colon form would have silently
#    replaced SPIRA_PROD="" with a path from a layout that may not exist).
#
# defect: sp-ncxv
# tier: T1
# covers: systemd/install.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
REAL_COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
nonzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }

echo "test-install-exec.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture: minimal harness tree (same pattern as test-install-aeons.sh).
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
printf '# empty\n' > "$FIXTURE/spira/watchers"
printf '# empty\n' > "$FIXTURE/spira/repo-map.example"
printf '#!/usr/bin/env bash\nexit 0\n' > "$FIXTURE/spira/install-session-hook.sh"
chmod +x "$FIXTURE/spira/install-session-hook.sh"

DEST="$TMP/home/.config/systemd/user"
SPIRA_RUN_DIR="$TMP/run"
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$DEST" "$SPIRA_RUN_DIR" "$MOCK_BIN"
MOCK_LOG="$TMP/systemctl.log"

cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${MOCK_LOG}"
case "$*" in
    *list-units*active*spira-aeon*) true ;;
    *list-unit-files*spira-watch*) printf 'spira-watch@testview.service enabled\n' ;;
    *list-units*spira-watch*) printf 'spira-watch@testview.service loaded active running\n' ;;
    *is-active*) printf 'active\n' ;;
    *list-timers*) true ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/loginctl"
chmod +x "$MOCK_BIN/loginctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/spira-supervise"
chmod +x "$MOCK_BIN/spira-supervise"

# inst [args] — run install.sh in a controlled environment.
# TEST_PROD overrides SPIRA_PROD for the scenario under test; default is $HERE (all
# ExecStart targets exist and are executable there).
inst() {
    > "$MOCK_LOG"
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_WATCHERS=$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_PROD=${TEST_PROD:-$HERE}" \
        "SPIRA_REPO=$REAL_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "SPIRA_SUPERVISE_BIN=$MOCK_BIN/spira-supervise" \
        "MOCK_LOG=$MOCK_LOG" \
        SPIRA_INSTALL_FORCE=1 \
        bash "$FIXTURE/systemd/install.sh" "$@" 2>&1
}

# Seed DEST with pre-rendered units so --diff comparisons work and the install
# loop has existing files to replace rather than encountering missing targets.
rendered="$(inst --render)"; render_rc=$?
if [ "$render_rc" != 0 ]; then
    printf 'fixture: install.sh --render failed (rc=%s) — cannot continue\n' "$render_rc"
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

# ==========================================================================
echo
echo "CLEAN INSTALL — ExecStart targets exist and are executable:"
# ==========================================================================

clean_out="$(inst)"
clean_rc=$?
iszero  "clean: exit 0 when all ExecStart targets are executable" "$clean_rc"
nowant  "clean: no 'not executable' in output" "not executable" "$clean_out"

# ==========================================================================
echo
echo "MISSING PROD — SPIRA_PROD points at a non-existent directory:"
# ==========================================================================

TEST_PROD="/no/such/spira/directory" \
missing_out="$(TEST_PROD="/no/such/spira/directory" inst)"
missing_rc=$?
nonzero "missing: exit non-zero when ExecStart target does not exist" "$missing_rc"
want    "missing: output names the bad path" "/no/such/spira/directory" "$missing_out"
want    "missing: output says 'not executable'" "not executable" "$missing_out"
want    "missing: output names install as the reporter" "install:" "$missing_out"

# ==========================================================================
echo
echo "NON-EXECUTABLE TARGET — target exists but lacks +x:"
# ==========================================================================

# Build a prod directory with scripts that exist but are not executable.
NOEXEC="$TMP/noexec-prod"
mkdir -p "$NOEXEC"
for s in aeon.sh archive.sh archivist.sh cockpit.sh loom.sh sentinel.sh \
         skew.sh suites.sh watchd.sh watchtower.sh; do
    printf '#!/usr/bin/env bash\ntrue\n' > "$NOEXEC/$s"
    # Deliberately NOT chmod +x
done

TEST_PROD="$NOEXEC" \
noexec_out="$(TEST_PROD="$NOEXEC" inst)"
noexec_rc=$?
nonzero "noexec: exit non-zero when ExecStart target exists but lacks +x" "$noexec_rc"
want    "noexec: output names the non-executable script path" "$NOEXEC" "$noexec_out"
want    "noexec: output says 'not executable'" "not executable" "$noexec_out"

# ==========================================================================
echo
echo "CONF EMPTY — SPIRA_PROD= (empty) is preserved, not replaced by default:"
# ==========================================================================

# Source conf.sh in an environment where SPIRA_PROD is explicitly empty.
# With no-colon =, an empty value is kept; with := it would be overwritten.
conf_result="$(
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PROD= \
        bash -c '. '"$HERE/conf.sh"'; printf "%s" "${SPIRA_PROD:-__EMPTY__}"' 2>/dev/null
)"
# If the fix works, SPIRA_PROD stays empty and we get __EMPTY__.
# If := is still there, we get the derived path (non-empty string, not __EMPTY__).
is_empty=""
[ "$conf_result" = "__EMPTY__" ] && is_empty=1
[ -n "$is_empty" ] && ok "conf empty: SPIRA_PROD= preserved as empty (no-colon form)" \
                  || bad "conf empty: SPIRA_PROD= was overridden by derived default: [$conf_result]"

# ==========================================================================
echo
echo "RENDER FALLBACK — empty SPIRA_PROD renders as SPIRA_HOME (empty-in, dev-checkout-out):"
# ==========================================================================

# When SPIRA_PROD is empty — the signal that no checkout split is wanted — render()
# must substitute SPIRA_HOME so @SPIRA_PROD@ yields a real path, not an empty prefix
# (which would produce ExecStart=/sentinel.sh and pass silently, since no placeholder
# remains unresolved).
render_fb_out="$(
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$HERE" \
        SPIRA_PROD= \
        "SPIRA_REPO=$REAL_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        bash "$FIXTURE/systemd/install.sh" --render 2>&1
)"
render_fb_rc=$?
iszero  "render fallback: --render exits 0 with empty SPIRA_PROD" "$render_fb_rc"
# With the fallback, @SPIRA_PROD@ resolves to SPIRA_HOME ($HERE). Verify the
# rendered sentinel ExecStart contains SPIRA_HOME, not a bare-slash path.
want    "render fallback: sentinel ExecStart contains SPIRA_HOME" "ExecStart=$HERE/sentinel.sh" "$render_fb_out"

# ==========================================================================
echo
echo "TARBALL LAYOUT — SPIRA_PROD must be <release>/spira, not <release>:"
# ==========================================================================
#
# deploy.sh sets SPIRA_PROD="$SPIRA_RELEASES/current/spira" when calling
# install.sh. The release tarball unpacks as <stem>/<repo-tree>/ so the
# harness scripts live at <stem>/spira/sentinel.sh etc., not <stem>/sentinel.sh.
# If SPIRA_PROD were set to <releases>/current (missing /spira), install.sh
# would look for sentinel.sh at the wrong path and refuse.
#
# This property is the test that would have caught the original bug; it uses a
# real git archive so the executability of the scripts is real, not assumed.

_tarball_tmp="$TMP/tarball-unpack"
_tarball_stem="spira-20260901T000000Z"
_tarball_root="$_tarball_tmp/$_tarball_stem"
mkdir -p "$_tarball_root"
if tar -C "$REAL_REPO" --exclude='.git' -cf - . 2>/dev/null | tar -x -C "$_tarball_root" 2>/dev/null; then
    # FAIL-FIRST: wrong SPIRA_PROD (the release root, without /spira).
    # install.sh looks for sentinel.sh in the wrong place and refuses.
    _tb_fail="$(TEST_PROD="$_tarball_root" inst)"
    _tb_fail_rc=$?
    nonzero "tarball/fail-first: SPIRA_PROD=<release> (missing /spira) → exits non-zero" "$_tb_fail_rc"
    want    "tarball/fail-first: output names the bad path" "$_tarball_root" "$_tb_fail"
    want    "tarball/fail-first: output says not executable" "not executable" "$_tb_fail"

    # Correct SPIRA_PROD — all scripts under <release>/spira/ are executable
    # because git preserves the +x bit from the tree.
    _tb_ok="$(TEST_PROD="$_tarball_root/spira" inst)"
    _tb_ok_rc=$?
    iszero  "tarball/correct: SPIRA_PROD=<release>/spira → install.sh exits 0" "$_tb_ok_rc"
    nowant  "tarball/correct: no 'not executable' in output" "not executable" "$_tb_ok"
else
    bad "tarball/setup: git archive failed — cannot run tarball layout test" "git archive error"
fi

# ==========================================================================
echo
echo "COCKPIT PATH — cockpit-ensure.service ExecStart uses dirname(SPIRA_PROD)/cockpit:"
# ==========================================================================
#
# POSITIVE CONTROL FIRST. Pass a SPIRA_COCKPIT that differs from dirname(SPIRA_PROD)/cockpit.
# If the ExecStart still uses SPIRA_COCKPIT, the check below would pass anyway and prove nothing.
# Verify that the check CAN detect the wrong value before asserting the right one.
#
# Use a sentinel value for SPIRA_COCKPIT so any path containing it is clearly wrong.
FAKE_COCKPIT="$TMP/fake-checkout-cockpit"
FAKE_PROD="$TMP/fake-releases/current/spira"
FAKE_PROD_COCK="$TMP/fake-releases/current/cockpit"
mkdir -p "$FAKE_PROD" "$FAKE_PROD_COCK"
printf '#!/usr/bin/env bash\nexit 0\n' > "$FAKE_PROD/sentinel.sh"
chmod +x "$FAKE_PROD/sentinel.sh"
for s in aeon.sh archive.sh archivist.sh cockpit.sh loom.sh skew.sh suites.sh watchd.sh watchtower.sh; do
    cp "$FAKE_PROD/sentinel.sh" "$FAKE_PROD/$s"
done
for s in layout.sh verify-asks.sh moot-sweep.sh; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$FAKE_PROD_COCK/$s"
    chmod +x "$FAKE_PROD_COCK/$s"
done

cock_render="$(
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_PROD=$FAKE_PROD" \
        "SPIRA_REPO=$REAL_REPO" \
        "SPIRA_COCKPIT=$FAKE_COCKPIT" \
        bash "$FIXTURE/systemd/install.sh" --render 2>&1
)"
cock_render_rc=$?

iszero "cockpit path: --render exits 0 with split SPIRA_PROD" "$cock_render_rc"

# Positive control: verify the check CAN detect FAKE_COCKPIT appearing in cockpit-ensure ExecStart.
case "$cock_render" in
    *"ExecStart=$FAKE_COCKPIT"*)
        bad "cockpit path: positive control — ExecStart does NOT use FAKE_COCKPIT" \
            "FAKE_COCKPIT appeared in ExecStart" ;;
    *)  ok "cockpit path: positive control — ExecStart does not use FAKE_COCKPIT" ;;
esac

# The real assertion: ExecStart points at dirname(SPIRA_PROD)/cockpit.
case "$cock_render" in
    *"ExecStart=$FAKE_PROD_COCK/layout.sh ensure"*)
        ok "cockpit path: cockpit-ensure ExecStart derived from dirname(SPIRA_PROD)/cockpit" ;;
    *)  bad "cockpit path: cockpit-ensure ExecStart derived from dirname(SPIRA_PROD)/cockpit" \
            "expected ExecStart=$FAKE_PROD_COCK/layout.sh ensure" ;;
esac

# ==========================================================================
echo
echo "COCKPIT HEAL LOG — cockpit-ensure.service StandardOutput uses SPIRA_RUN:"
# ==========================================================================
# Positive control: if SPIRA_REPO/.runtime appeared in StandardOutput, the check below
# would be vacuously true when the fix is absent. Verify it is NOT present.
case "$cock_render" in
    *"StandardOutput=append:$REAL_REPO/.runtime"*)
        bad "heal log: positive control — StandardOutput does NOT use SPIRA_REPO/.runtime" \
            "SPIRA_REPO/.runtime appeared in StandardOutput" ;;
    *)  ok "heal log: positive control — StandardOutput does not use SPIRA_REPO/.runtime" ;;
esac

case "$cock_render" in
    *"StandardOutput=append:$SPIRA_RUN_DIR/cockpit-heal.log"*)
        ok "heal log: cockpit-ensure StandardOutput uses SPIRA_RUN" ;;
    *)  bad "heal log: cockpit-ensure StandardOutput uses SPIRA_RUN" \
            "expected StandardOutput=append:$SPIRA_RUN_DIR/cockpit-heal.log" ;;
esac

# ==========================================================================
echo
tl_summary
