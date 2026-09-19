#!/usr/bin/env bash
#
# test-install-mail-deliver.sh — install skips spira-mail-deliver when inotifywait absent.
#
# WHAT THIS SUITE PROVES
# ----------------------
# Without inotifywait, spira-mail-deliver.sh exits 1 immediately, and the unit crash-loops
# under Restart=always. The fix: units.sh omits the unit when inotifywait is absent, so
# install.sh never installs or enables it. (sp-va1ej)
#
# TWO PROPERTIES are verified, both required for the fix to hold:
#
#   A  POSITIVE CONTROL: with inotifywait on PATH, install.sh --render includes
#      spira-mail-deliver in the rendered output.
#
#   B  SKIP: with inotifywait absent, install.sh --render produces no mail-deliver unit.
#      Without this, the crash-loop defense is inert.
#
# covers: systemd/units.sh systemd/install.sh systemd/spira-mail-deliver.service
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

echo "test-install-mail-deliver.sh"

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
MOCK_WITH="$TMP/mock-with"    # has inotifywait
MOCK_WITHOUT="$TMP/mock-without"    # no inotifywait
mkdir -p "$DEST" "$SPIRA_RUN_DIR" "$MOCK_BIN" "$MOCK_WITH" "$MOCK_WITHOUT"

for d in "$MOCK_BIN" "$MOCK_WITH" "$MOCK_WITHOUT"; do
    cat > "$d/systemctl" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *is-active*)  printf 'active\n' ;;
    *is-enabled*) printf 'enabled\n' ;;
    *"list-unit-files"*|*"list-units"*|*"list-timers"*) : ;;
esac
exit 0
MOCK
    chmod +x "$d/systemctl"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$d/loginctl"
    chmod +x "$d/loginctl"
done

# inotifywait stub present in MOCK_WITH only.
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_WITH/inotifywait"
chmod +x "$MOCK_WITH/inotifywait"

# PATH WITHOUT inotifywait. The container may have inotifywait at e.g. /usr/bin/inotifywait.
# Compute a filtered PATH that excludes any directory containing it, so the negative test
# case never finds the binary through the system PATH.
_iw_dir=""
_iw_bin="$(command -v inotifywait 2>/dev/null || true)"
[ -n "$_iw_bin" ] && _iw_dir="$(dirname "$_iw_bin")"
_path_no_iw=""
while IFS= read -r _pd; do
    [ -n "$_pd" ] || continue
    [ "$_pd" = "$_iw_dir" ] && continue
    _path_no_iw="${_path_no_iw:+${_path_no_iw}:}$_pd"
done <<< "$(printf '%s' "$PATH" | tr ':' '\n')"
unset _iw_bin _iw_dir _pd

render() {  # render <mock-bin-dir> <path>
    env -i \
        "PATH=${2:-$PATH}" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$1" \
        "SPIRA_WATCHERS=$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA= \
        SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_PROD=$HERE" \
        "SPIRA_REPO=$REAL_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        SPIRA_INSTALL_FORCE=1 \
        bash "$FIXTURE/systemd/install.sh" --render 2>&1
}

# ==========================================================================
echo
echo "A: POSITIVE CONTROL — inotifywait present → mail-deliver unit is rendered:"
# ==========================================================================
pos_out="$(render "$MOCK_WITH" "$MOCK_WITH:$PATH")"; pos_rc=$?
is     "A: render exits 0 with inotifywait present"               "0" "$pos_rc"
want   "A: mail-deliver service in rendered units" "spira-mail-deliver" "$pos_out"

# ==========================================================================
echo
echo "B: SKIP — inotifywait absent → no mail-deliver unit rendered:"
# ==========================================================================
neg_out="$(render "$MOCK_WITHOUT" "$MOCK_WITHOUT:$_path_no_iw")"; neg_rc=$?
is     "B: render exits 0 with inotifywait absent"                    "0" "$neg_rc"
nowant "B: mail-deliver absent when inotifywait not found" "spira-mail-deliver" "$neg_out"

# ==========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
