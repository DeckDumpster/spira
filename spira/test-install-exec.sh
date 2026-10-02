#!/usr/bin/env bash
#
# test-install-exec.sh — units-install refuses to write a unit whose ExecStart target is not
# executable, conf.sh preserves an explicitly-empty SPIRA_PROD, and --render falls back to
# SPIRA_HOME when SPIRA_PROD is empty.
#
#   ./test-install-exec.sh
#
# Every scenario pins SPIRA_WATCHERS to a fixture manifest: the render fallback once left it
# to default from SPIRA_HOME, so the manifest came from whatever tree the suite ran in.
#
# defect: sp-ncxv, sp-osl2c
# tier: T1
# covers: install/src/** systemd/*.service systemd/*.timer spira/conf.sh spira/lib-test-install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/lib-test-install.sh"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
REAL_COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
nonzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }

echo "test-install-exec.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

FIXTURE="$TMP/harness"
install_fixture_build "$FIXTURE"
SPIRA_RUN_DIR="$TMP/run"
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$TMP/home/.config/systemd/user" "$SPIRA_RUN_DIR" "$MOCK_BIN"
PROD="$(install_fixture_prod "$TMP/prod" "$HERE")"
MOCK_LOG="$TMP/systemctl.log"

cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${MOCK_LOG}"
case "$*" in
    *list-units*active*spira-aeon*) true ;;
    *is-active*) printf 'active\n' ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"
for t in loginctl spira-supervise; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/$t"; chmod +x "$MOCK_BIN/$t"
done

# inst [args] — units-install in a minimal environment; TEST_PROD overrides SPIRA_PROD.
inst() {
    > "$MOCK_LOG"
    env -i \
        "PATH=$MOCK_BIN:$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_WATCHERS=$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_PROD=${TEST_PROD-$PROD}" \
        "SPIRA_REPO=$REAL_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "MOCK_LOG=$MOCK_LOG" \
        SPIRA_INSTALL_FORCE=1 \
        units-install "$@" 2>&1
}

echo
echo "CLEAN INSTALL — ExecStart targets exist and are executable:"
clean_out="$(inst)"; clean_rc=$?
iszero  "clean: exit 0 when all ExecStart targets are executable" "$clean_rc"
nowant  "clean: no 'not executable' in output" "not executable" "$clean_out"

echo
echo "MISSING PROD — SPIRA_PROD points at a non-existent directory:"
missing_out="$(TEST_PROD=/no/such/spira/directory inst)"; missing_rc=$?
nonzero "missing: exit non-zero when ExecStart target does not exist" "$missing_rc"
want    "missing: output names the bad path" "/no/such/spira" "$missing_out"
want    "missing: output says 'not executable'" "not executable" "$missing_out"

echo
echo "NON-EXECUTABLE TARGET — target exists but lacks +x:"
NOEXEC="$(install_fixture_prod "$TMP/noexec-root" "$HERE")"
find "$TMP/noexec-root/bin" -type f -exec chmod -x {} +
noexec_out="$(TEST_PROD="$NOEXEC" inst)"; noexec_rc=$?
nonzero "noexec: exit non-zero when ExecStart target exists but lacks +x" "$noexec_rc"
want    "noexec: output names the non-executable path" "$TMP/noexec-root" "$noexec_out"
want    "noexec: output says 'not executable'" "not executable" "$noexec_out"

echo
echo "CONF EMPTY — SPIRA_PROD= (empty) is preserved, not replaced by default:"
conf_result="$(
    env -i "PATH=$PATH" "HOME=$TMP/home" SPIRA_CONF=/nonexistent SPIRA_PROD= \
        bash -c '. '"$HERE/conf.sh"'; printf "%s" "${SPIRA_PROD:-__EMPTY__}"' 2>/dev/null
)"
[ "$conf_result" = "__EMPTY__" ] && ok "conf empty: SPIRA_PROD= preserved as empty" \
                                 || bad "conf empty: SPIRA_PROD= was overridden: [$conf_result]"

echo
echo "RENDER FALLBACK — empty SPIRA_PROD renders as SPIRA_HOME:"
render_fb_out="$(TEST_PROD= inst --render)"; render_fb_rc=$?
iszero "render fallback: --render exits 0 with empty SPIRA_PROD" "$render_fb_rc"
nowant "render fallback: the watcher manifest was read" "manifest is malformed" "$render_fb_out"
want   "render fallback: sentinel ExecStart derives from SPIRA_HOME" "ExecStart=$(dirname "$HERE")/bin/sentinel" "$render_fb_out"

tl_summary
