#!/usr/bin/env bash
#
# test-install-bootstrap-release.sh — install.sh phase 4 bootstraps SPIRA_PROD on a
# fresh box when SPIRA_PROD resolves under SPIRA_RELEASES, and refuses when it does not.
#
#   ./test-install-bootstrap-release.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. BOOTSTRAP: SPIRA_PROD under SPIRA_RELEASES and absent → install creates
#    SPIRA_RELEASES/bootstrap/, SPIRA_RELEASES/current -> bootstrap, exits 0 or 3.
# 2. REFUSE: SPIRA_PROD absent and outside SPIRA_RELEASES → exits 2, names "activate.sh".
#
# POSITIVE CONTROL: the refuse case (test 2) is verified first so a silent bootstrap
# is believed (law-absence-needs-a-positive-control).
#
# covers: install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is2()    { [ "$2" = 2 ] && ok "$1" || bad "$1" "wanted exit 2, got $2"; }
not2()   { [ "$2" != 2 ] && ok "$1" || { bad "$1" "must not exit 2"; printf '  install output:\n'; printf '%s\n' "${3:-}" | sed 's/^/    /'; }; }
islink() { [ -L "$2" ] && ok "$1" || bad "$1" "expected symlink at $2"; }
isdir()  { [ -d "$2" ] && ok "$1" || bad "$1" "expected directory at $2"; }

echo "test-install-bootstrap-release.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture — minimal harness tree used as the "clone" install.sh runs from.
# All ExecStart @SPIRA_PROD@ scripts are stubbed here so that after bootstrap
# copies this tree into SPIRA_RELEASES/bootstrap, systemd/install.sh can verify
# the rendered unit paths without failing.
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
SPIRA_DIR="$FIXTURE/spira"
SYSTEMD_DIR="$FIXTURE/systemd"
COCKPIT_DIR="$FIXTURE/cockpit"
mkdir -p "$SPIRA_DIR" "$SYSTEMD_DIR" "$COCKPIT_DIR"

for f in "$REAL_REPO/systemd/"*.service "$REAL_REPO/systemd/"*.timer "$REAL_REPO/systemd/"*.yaml; do
    [ -e "$f" ] || continue
    ln -s "$f" "$SYSTEMD_DIR/$(basename "$f")" 2>/dev/null || true
done
ln -s "$REAL_REPO/systemd/install.sh" "$SYSTEMD_DIR/install.sh"
ln -s "$REAL_REPO/systemd/units.sh"   "$SYSTEMD_DIR/units.sh"

ln -s "$HERE/conf.sh"         "$SPIRA_DIR/conf.sh"
ln -s "$HERE/lib.sh"          "$SPIRA_DIR/lib.sh"
ln -s "$HERE/suite-covers.sh" "$SPIRA_DIR/suite-covers.sh"
ln -s "$HERE/watchd.sh"       "$SPIRA_DIR/watchd.sh"
printf '# empty\n' > "$SPIRA_DIR/watchers"
printf '# empty\n' > "$SPIRA_DIR/repo-map.example"
mkdir -p "$SPIRA_DIR/statutes"

# Executable stubs for every ExecStart under @SPIRA_PROD@ and @SPIRA_HOME@ so
# systemd/install.sh's executability check passes after bootstrap copies this tree.
for _s in $(grep -h "ExecStart=\|ExecStartPre=" "$REAL_REPO/systemd/"*.service 2>/dev/null \
            | grep "@SPIRA_PROD@" | sed 's|.*@SPIRA_PROD@/||' | sed 's/ .*//' | sort -u); do
    [ -e "$SPIRA_DIR/$_s" ] || { printf '#!/usr/bin/env bash\nexit 0\n' > "$SPIRA_DIR/$_s"; chmod +x "$SPIRA_DIR/$_s"; }
done
for _s in $(grep -h "ExecStart=\|ExecStartPre=" "$REAL_REPO/systemd/"*.service 2>/dev/null \
            | grep "@SPIRA_HOME@" | sed 's|.*@SPIRA_HOME@/||' | sed 's/ .*//' | sort -u); do
    [ -e "$SPIRA_DIR/$_s" ] || { printf '#!/usr/bin/env bash\nexit 0\n' > "$SPIRA_DIR/$_s"; chmod +x "$SPIRA_DIR/$_s"; }
done
unset _s

# Executable stubs for @SPIRA_COCKPIT@ scripts.
for _s in $(grep -h "ExecStart=\|ExecStartPre=" "$REAL_REPO/systemd/"*.service 2>/dev/null \
            | grep "@SPIRA_COCKPIT@" | sed 's|.*@SPIRA_COCKPIT@/||' | sed 's/ .*//' | sort -u); do
    [ -e "$COCKPIT_DIR/$_s" ] || { printf '#!/usr/bin/env bash\nexit 0\n' > "$COCKPIT_DIR/$_s"; chmod +x "$COCKPIT_DIR/$_s"; }
done
unset _s

# Executable stubs for @SPIRA_PROD_COCK@ scripts. SPIRA_PROD_COCK resolves as
# dirname(SPIRA_PROD)/cockpit; after bootstrap that is inside the release tree,
# which is a copy of $FIXTURE. Stubs placed in $COCKPIT_DIR (= $FIXTURE/cockpit)
# land there automatically.
for _s in $(grep -h "ExecStart=\|ExecStartPre=" "$REAL_REPO/systemd/"*.service 2>/dev/null \
            | grep "@SPIRA_PROD_COCK@" | sed 's|.*@SPIRA_PROD_COCK@/||' | sed 's/ .*//' | sort -u); do
    [ -e "$COCKPIT_DIR/$_s" ] || { printf '#!/usr/bin/env bash\nexit 0\n' > "$COCKPIT_DIR/$_s"; chmod +x "$COCKPIT_DIR/$_s"; }
done
unset _s

# Required stubs for install.sh phases.
cat > "$SPIRA_DIR/doctor.sh" <<'EOF'
#!/usr/bin/env bash
echo "spira doctor"
echo "  ok    stub — all checks passed"
exit 0
EOF
chmod +x "$SPIRA_DIR/doctor.sh"

cat > "$SPIRA_DIR/build.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$SPIRA_DIR/build.sh"

cat > "$SPIRA_DIR/seed.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$SPIRA_DIR/seed.sh"

cat > "$SPIRA_DIR/install-session-hook.sh" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
    status)  printf 'ok  SessionStart\n' ;;
    install) printf 'installed\n' ;;
    *)       exit 0 ;;
esac
exit 0
EOF
chmod +x "$SPIRA_DIR/install-session-hook.sh"

cat > "$SPIRA_DIR/install-intake.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$SPIRA_DIR/install-intake.sh"

cat > "$SPIRA_DIR/ready.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$SPIRA_DIR/ready.sh"

cat > "$SPIRA_DIR/exclude.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$SPIRA_DIR/exclude.sh"

# configure.sh stub: writes a minimal conf file respecting CONFIGURE_PROD.
cat > "$SPIRA_DIR/configure.sh" <<'EOF'
#!/usr/bin/env bash
_out="${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf"
if [ -f "$_out" ]; then exit 1; fi
mkdir -p "$(dirname "$_out")"
printf 'SPIRA_PROD = %s\n' "${CONFIGURE_PROD:-${SPIRA_PROD:-/nonexistent}}" > "$_out"
EOF
chmod +x "$SPIRA_DIR/configure.sh"

ln -s "$REAL_REPO/install.sh" "$FIXTURE/install.sh"

# Mock system binaries.
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$MOCK_BIN"
MOCK_LOG="$TMP/mock.log"

cat > "$MOCK_BIN/systemctl" <<EOF
#!/usr/bin/env bash
printf 'systemctl %s\n' "\$*" >> "${MOCK_LOG}"
case "\$*" in
    *is-active*) echo "inactive" ;;
    *list-unit-files*spira-watch*) printf 'spira-watch@testview.service enabled\n' ;;
    *list-units*spira-watch*)  printf 'spira-watch@testview.service loaded active running\n' ;;
    *list-units*active*spira-aeon*) true ;;
    *list-timers*) true ;;
esac
exit 0
EOF
chmod +x "$MOCK_BIN/systemctl"

cat > "$MOCK_BIN/loginctl" <<'EOF'
#!/usr/bin/env bash
case "$*" in *show-user*Linger*) echo "Linger=no" ;; esac
exit 0
EOF
chmod +x "$MOCK_BIN/loginctl"

cat > "$MOCK_BIN/tmux" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$MOCK_BIN/tmux"

cat > "$MOCK_BIN/bd" <<'EOF'
#!/usr/bin/env bash
case "$*" in
    *init*)     exit 0 ;;
    *list*)     printf '[]\n' ;;
    *memories*) printf '{}\n' ;;
    *)          exit 0 ;;
esac
EOF
chmod +x "$MOCK_BIN/bd"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/spira-supervise"
chmod +x "$MOCK_BIN/spira-supervise"

# Fake git repo for SPIRA_REPO.
FAKE_ORIGIN="$TMP/origin.git"
FAKE_REPO="$TMP/fakerepo"
git init -q --bare -b main "$FAKE_ORIGIN" 2>/dev/null
git init -q -b main "$FAKE_REPO" 2>/dev/null
git -C "$FAKE_REPO" config user.email t@t
git -C "$FAKE_REPO" config user.name test
printf 'seed\n' > "$FAKE_REPO/f"
git -C "$FAKE_REPO" add f
git -C "$FAKE_REPO" commit -qm "seed" 2>/dev/null
git -C "$FAKE_REPO" remote add origin "$FAKE_ORIGIN"
git -C "$FAKE_REPO" push -q origin main 2>/dev/null
git -C "$FAKE_REPO" fetch -q origin 2>/dev/null
git -C "$FAKE_REPO" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main
# @SPIRA_REPO@ stubs.
for _s in $(grep -h "ExecStart=\|ExecStartPre=" "$REAL_REPO/systemd/"*.service 2>/dev/null \
            | grep "@SPIRA_REPO@" | sed 's|.*@SPIRA_REPO@/||' | sed 's/ .*//' | sort -u); do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$FAKE_REPO/$_s"; chmod +x "$FAKE_REPO/$_s"
done
unset _s

FAKE_HOME="$TMP/home"
FAKE_UNITDIR="$FAKE_HOME/.config/systemd/user"
FAKE_RUN="$TMP/run"
FAKE_DB="$TMP/db"
mkdir -p "$FAKE_HOME" "$FAKE_UNITDIR" "$FAKE_RUN" "$FAKE_DB"

run_install() {
    local extra_env=() install_args=() in_env=0
    for _a in "$@"; do
        [ "$_a" = "--" ] && { in_env=1; continue; }
        [ "$in_env" = 1 ] && { extra_env+=("$_a"); continue; }
        install_args+=("$_a")
    done
    unset _a in_env
    > "$MOCK_LOG"
    env -i \
        "PATH=$MOCK_BIN:$PATH" \
        "HOME=$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_WATCHERS=$SPIRA_DIR/watchers" \
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$FAKE_RUN" \
        "SPIRA_HOME=$SPIRA_DIR" \
        "SPIRA_REPO=$FAKE_REPO" \
        "SPIRA_COCKPIT=$COCKPIT_DIR" \
        "SPIRA_DB=$FAKE_DB" \
        "MOCK_LOG=$MOCK_LOG" \
        SPIRA_INSTALL_FORCE=1 \
        SPIRA_INSTALL_CONFLICT_CONSIDERED=1 \
        "SPIRA_BD=$MOCK_BIN/bd" \
        "SPIRA_SUPERVISE_BIN=$MOCK_BIN/spira-supervise" \
        "${extra_env[@]+"${extra_env[@]}"}" \
        bash "$FIXTURE/install.sh" \
            --skip-build \
            "${install_args[@]+"${install_args[@]}"}" 2>&1
}

# ===========================================================================
echo
echo "POSITIVE CONTROL: SPIRA_PROD outside SPIRA_RELEASES refuses with activate.sh"
# ===========================================================================
# Verify the guard fires before asserting that a missing release dir bootstraps.
# Use --dry-run: _phase_fail exits 2 unconditionally.
OUTSIDE_RELEASES="$TMP/outside/spira"
FAKE_RELEASES_A="$TMP/releases-a"
mkdir -p "$FAKE_RELEASES_A"

_refuse_out="$(run_install prod --dry-run -- \
    "SPIRA_RELEASES=$FAKE_RELEASES_A" \
    "SPIRA_PROD=$OUTSIDE_RELEASES" \
    SPIRA_INSTALL_PROD_GIT_CONSIDERED=1)"
_refuse_rc=$?
is2   "refuse: exits 2"                    "$_refuse_rc"
want  "refuse: names 'activate.sh'"        "activate.sh" "$_refuse_out"
want  "refuse: names SPIRA_PROD path"      "$OUTSIDE_RELEASES" "$_refuse_out"

# ===========================================================================
echo
echo "BOOTSTRAP: SPIRA_PROD under SPIRA_RELEASES absent → release dir + current symlink"
# ===========================================================================
# Fresh SPIRA_RELEASES with no current symlink and no SPIRA_PROD set.
# install.sh derives SPIRA_PROD = SPIRA_RELEASES/current/spira and bootstraps it.
FAKE_RELEASES_B="$TMP/releases-b"
mkdir -p "$FAKE_RELEASES_B"
# Remove the home conf so configure.sh stub can write a fresh one.
rm -f "$FAKE_HOME/.config/spira/spira.conf"

# world.halted bypasses the unit-active check in systemd/install.sh — units cannot
# start in a test environment and that is not what this test covers.
touch "$FAKE_RUN/world.halted"
_boot_out="$(run_install prod -- "SPIRA_RELEASES=$FAKE_RELEASES_B" 2>&1)"
_boot_rc=$?
not2  "bootstrap: install does not exit 2"                    "$_boot_rc" "$_boot_out"
islink "bootstrap: SPIRA_RELEASES/current is a symlink"       "$FAKE_RELEASES_B/current"
isdir  "bootstrap: SPIRA_RELEASES/bootstrap/ exists"          "$FAKE_RELEASES_B/bootstrap"
isdir  "bootstrap: SPIRA_RELEASES/bootstrap/spira/ exists"    "$FAKE_RELEASES_B/bootstrap/spira"
isdir  "bootstrap: SPIRA_RELEASES/current/spira resolves"     "$FAKE_RELEASES_B/current/spira"
_cur_target="$(readlink "$FAKE_RELEASES_B/current" 2>/dev/null || true)"
[ "$_cur_target" = "bootstrap" ] \
    && ok  "bootstrap: current -> bootstrap" \
    || bad "bootstrap: current target" "wanted 'bootstrap', got '$_cur_target'"

# ===========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
