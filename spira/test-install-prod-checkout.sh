#!/usr/bin/env bash
#
# test-install-prod-checkout.sh — install.sh CONFIGURE_PROD and SPIRA_PROD guards.
#
#   ./test-install-prod-checkout.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. GIT CHECKOUT: SPIRA_PROD is a git checkout → exit 2, names "git checkout",
#    names the override.
# 2. OVERRIDE: SPIRA_INSTALL_PROD_GIT_CONSIDERED=1 bypasses the guard → no
#    "is a git checkout" in output.
# 3. CLEAN: SPIRA_PROD is not a git checkout → guard does not fire.
# 4. CONFIGURE_PROD WITHOUT conf.sh: exit 1, names "conf.sh", names "/spira" path.
# 5. CONFIGURE_PROD WITH conf.sh: guard does not fire.
#
# FAIL-FIRST: the git-checkout case is verified first so a silent clean case
# is believed (law-absence-needs-a-positive-control).
#
# covers: install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is2()     { [ "$2" = 2 ] && ok "$1" || bad "$1" "wanted exit 2, got $2"; }
not2()    { [ "$2" != 2 ] && ok "$1" || bad "$1" "must not exit 2, got 2"; }

echo "test-install-prod-checkout.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture — same structure as test-install-conflicts.sh.
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
SPIRA_DIR="$FIXTURE/spira"
SYSTEMD_DIR="$FIXTURE/systemd"
COCKPIT_DIR="$FIXTURE/cockpit"
mkdir -p "$SPIRA_DIR" "$SYSTEMD_DIR" "$COCKPIT_DIR"

for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer; do
    [ -e "$f" ] || continue
    ln -s "$f" "$SYSTEMD_DIR/$(basename "$f")" 2>/dev/null || true
done
ln -s "$HERE/../systemd/install.sh" "$SYSTEMD_DIR/install.sh"
ln -s "$HERE/../systemd/units.sh"   "$SYSTEMD_DIR/units.sh"

ln -s "$HERE/conf.sh"   "$SPIRA_DIR/conf.sh"
ln -s "$HERE/lib.sh"    "$SPIRA_DIR/lib.sh"
ln -s "$HERE/watchd.sh" "$SPIRA_DIR/watchd.sh"

printf '# empty\n' > "$SPIRA_DIR/watchers"
printf '# empty\n' > "$SPIRA_DIR/repo-map.example"
mkdir -p "$SPIRA_DIR/statutes"

cat > "$SPIRA_DIR/doctor.sh" <<'EOF'
#!/usr/bin/env bash
echo "spira doctor"
echo "  ok    stub — all checks passed"
exit 0
EOF
chmod +x "$SPIRA_DIR/doctor.sh"

cat > "$SPIRA_DIR/configure.sh" <<'EOF'
#!/usr/bin/env bash
_out="${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf"
if [ -f "$_out" ]; then exit 1; fi
mkdir -p "$(dirname "$_out")"
printf 'SPIRA_PROD = %s\n' "${CONFIGURE_PROD:-/nonexistent}" > "$_out"
EOF
chmod +x "$SPIRA_DIR/configure.sh"

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
exit 1
EOF
chmod +x "$SPIRA_DIR/ready.sh"

cat > "$COCKPIT_DIR/layout.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$COCKPIT_DIR/layout.sh"

ln -s "$REAL_REPO/install.sh" "$FIXTURE/install.sh"

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

FAKE_HOME="$TMP/home"
FAKE_UNITDIR="$FAKE_HOME/.config/systemd/user"
FAKE_RUN="$TMP/run"
FAKE_DB="$TMP/db"
# SPIRA_RELEASES points at a non-git directory: the default SPIRA_PROD derives from it.
FAKE_RELEASES="$TMP/releases"
mkdir -p "$FAKE_HOME" "$FAKE_UNITDIR" "$FAKE_RUN" "$FAKE_DB" "$FAKE_RELEASES"

# Pre-seed FAKE_UNITDIR so --diff in the unit installer has something to compare.
_rendered="$(env -i \
    "PATH=$MOCK_BIN:$PATH" \
    "HOME=$FAKE_HOME" \
    SPIRA_CONF=/nonexistent \
    "SPIRA_PATH=$MOCK_BIN" \
    "SPIRA_WATCHERS=$SPIRA_DIR/watchers" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    "SPIRA_RUN=$FAKE_RUN" \
    "SPIRA_HOME=$SPIRA_DIR" \
    "SPIRA_PROD=$SPIRA_DIR" \
    "SPIRA_REPO=$FAKE_REPO" \
    "SPIRA_COCKPIT=$COCKPIT_DIR" \
    "MOCK_LOG=$MOCK_LOG" \
    SPIRA_INSTALL_FORCE=1 \
    "SPIRA_BD=$MOCK_BIN/bd" \
    bash "$SYSTEMD_DIR/install.sh" prod --render 2>/dev/null)"
_render_rc=$?
if [ "$_render_rc" = 0 ]; then
    _cur=""
    while IFS= read -r _line; do
        if [[ "$_line" =~ ^=====\ (.+)\ =====$ ]]; then
            _cur="${BASH_REMATCH[1]}"; > "$FAKE_UNITDIR/$_cur"
        elif [ -n "$_cur" ]; then
            printf '%s\n' "$_line" >> "$FAKE_UNITDIR/$_cur"
        fi
    done <<< "$_rendered"
    unset _cur _line
fi
unset _rendered _render_rc

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
        "SPIRA_PROD=$SPIRA_DIR" \
        "SPIRA_RELEASES=$FAKE_RELEASES" \
        "SPIRA_REPO=$FAKE_REPO" \
        "SPIRA_COCKPIT=$COCKPIT_DIR" \
        "MOCK_LOG=$MOCK_LOG" \
        SPIRA_INSTALL_FORCE=1 \
        "SPIRA_BD=$MOCK_BIN/bd" \
        "${extra_env[@]+"${extra_env[@]}"}" \
        bash "$FIXTURE/install.sh" "${install_args[@]+"${install_args[@]}"}" 2>&1
}

# ---------------------------------------------------------------------------
# Create a git checkout directory to use as SPIRA_PROD.
# ---------------------------------------------------------------------------
GIT_PROD="$TMP/git-checkout"
mkdir -p "$GIT_PROD"
git init -q "$GIT_PROD" 2>/dev/null
git -C "$GIT_PROD" config user.email t@t
git -C "$GIT_PROD" config user.name test

# ===========================================================================
echo
echo "POSITIVE CONTROL: git checkout at SPIRA_PROD fires the guard"
# ===========================================================================
_git_out="$(run_install prod -- "SPIRA_PROD=$GIT_PROD")"
_git_rc=$?
is2   "git-checkout: exits 2"                         "$_git_rc"
want  "git-checkout: names 'git checkout'"            "git checkout" "$_git_out"
want  "git-checkout: names the override"              "SPIRA_INSTALL_PROD_GIT_CONSIDERED" "$_git_out"
want  "git-checkout: names SPIRA_PROD path"           "$GIT_PROD" "$_git_out"

# ===========================================================================
echo
echo "OVERRIDE: SPIRA_INSTALL_PROD_GIT_CONSIDERED=1 bypasses the guard"
# ===========================================================================
_ov_out="$(run_install prod -- "SPIRA_PROD=$GIT_PROD" SPIRA_INSTALL_PROD_GIT_CONSIDERED=1)"
_ov_rc=$?
nowant "override: does not mention 'is a git checkout'" "is a git checkout" "$_ov_out"

# ===========================================================================
echo
echo "CLEAN: non-git SPIRA_PROD does not trigger the guard"
# ===========================================================================
# Create a non-git directory with the scripts systemd/install.sh checks.
CLEAN_PROD="$TMP/clean-prod"
mkdir -p "$CLEAN_PROD"
for _s in aeon.sh archive.sh archivist.sh cockpit.sh loom.sh sentinel.sh \
          skew.sh suites.sh watchd.sh watchtower.sh; do
    printf '#!/usr/bin/env bash\ntrue\n' > "$CLEAN_PROD/$_s"
    chmod +x "$CLEAN_PROD/$_s"
done
unset _s

_clean_out="$(run_install prod -- "SPIRA_PROD=$CLEAN_PROD" SPIRA_INSTALL_CONFLICT_CONSIDERED=1)"
nowant "clean: does not mention 'is a git checkout'" "is a git checkout" "$_clean_out"

# ===========================================================================
echo
echo "CONFIGURE_PROD without conf.sh fires the guard"
# ===========================================================================
NO_CONF_DIR="$TMP/no-conf"
mkdir -p "$NO_CONF_DIR"

_no_conf_rc=0
_no_conf_out="$(run_install prod -- "CONFIGURE_PROD=$NO_CONF_DIR" SPIRA_INSTALL_CONFLICT_CONSIDERED=1 2>&1)" || _no_conf_rc=$?
[ "$_no_conf_rc" -ne 0 ] \
    && ok "CONFIGURE_PROD without conf.sh: exits non-zero" \
    || bad "CONFIGURE_PROD without conf.sh: exits non-zero" "exit 0 (expected non-zero)"
want "CONFIGURE_PROD without conf.sh: names conf.sh"  "conf.sh"            "$_no_conf_out"
want "CONFIGURE_PROD without conf.sh: names /spira"   "/spira"             "$_no_conf_out"
want "CONFIGURE_PROD without conf.sh: names the path" "$NO_CONF_DIR"       "$_no_conf_out"

# ===========================================================================
echo
echo "CONFIGURE_PROD with conf.sh does not fire the guard"
# ===========================================================================
# SPIRA_DIR already has conf.sh symlinked in — use it as a valid CONFIGURE_PROD.
_with_conf_out="$(run_install prod --dry-run -- "CONFIGURE_PROD=$SPIRA_DIR" SPIRA_INSTALL_CONFLICT_CONSIDERED=1 2>&1)" || true
nowant "CONFIGURE_PROD with conf.sh: does not mention 'does not contain conf.sh'" \
    "does not contain conf.sh" "$_with_conf_out"

# ===========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
