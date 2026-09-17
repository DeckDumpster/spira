#!/usr/bin/env bash
#
# test-deploy.sh — deploy.sh: fetch release, drain, activate, re-render, health check,
#                             rollback on failure, --dry-run
#
# PROPERTIES
#   1. Self-check: deploy.sh must exist.
#   2. (a) Basic deploy: latest tag resolves; gh fetches tarball; current points at release.
#   3. (a) Already-current: refuses (exit 1) if the release is already active.
#   4. (b) Drain refuses: non-zero drain exit blocks deploy without killing aeons.
#   5. (c) Rollback: failed health check restores the prior release.
#   6. (d) Unit re-render: install.sh is called with SPIRA_PROD=$SPIRA_RELEASES/current.
#   7. (e) Dry-run: --dry-run leaves the releases dir untouched.
#
# FAIL-FIRST (law-absence-needs-a-positive-control)
# Each detector is shown to fire before it is trusted as silent.
#
# covers: spira/deploy.sh spira/conf.sh
# host-reason: mock components in isolated temp dirs; no real systemd, database, or network
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is0()     { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }
not0()    { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero, got 0"; }
islink()  {
    if [ -L "$2" ] && [ "$(readlink "$2")" = "$3" ]; then
        ok "$1"
    else
        bad "$1" "wanted symlink $2 -> $3 (got: $(readlink "$2" 2>/dev/null || echo '(missing)'))"
    fi
}

echo "test-deploy.sh"

DEPLOY="$HERE/deploy.sh"

# --- PROPERTY 1: self-check ---------------------------------------------------
if [ ! -x "$DEPLOY" ]; then
    bad "self-check: deploy.sh must exist and be executable" "missing"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"; exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

RELEASES="$TMP/releases"
RUN_DIR="$TMP/run"
BIN="$TMP/bin"
CALL_LOG="$TMP/calls.log"
SC_LOG="$TMP/sc.log"
mkdir -p "$RELEASES" "$RUN_DIR" "$BIN"

NEW_RELEASE="spira-20260917T053803Z"
NEW_TAG="spira-release-$NEW_RELEASE"
PRIOR_RELEASE="spira-20260901T000000Z"

# Fake git repo with release tag for "latest" resolution.
FAKE_REPO="$TMP/repo"
git init -q "$FAKE_REPO" 2>/dev/null || true
git -C "$FAKE_REPO" -c user.email=t@t -c user.name=t \
    commit --allow-empty -q -m "init" 2>/dev/null || true
git -C "$FAKE_REPO" tag "$NEW_TAG" 2>/dev/null || true

# Mock gh: records calls; creates the tarball file in --dir.
cat > "$BIN/gh" <<'GHEOF'
#!/usr/bin/env bash
printf 'gh %s\n' "$*" >> "${CALL_LOG:-/dev/null}"
[ "${GH_EXIT:-0}" = "0" ] || exit "${GH_EXIT}"
_dir=""; _pat=""
while [ $# -gt 0 ]; do
    case "$1" in --dir) _dir="$2"; shift 2 ;; --pattern) _pat="$2"; shift 2 ;; *) shift ;; esac
done
[ -n "$_dir" ] || exit 1
mkdir -p "$_dir"
_name="${_pat%.tar.gz}"
mkdir -p "$_dir/.stage/$_name/spira"
printf '# stub\n' > "$_dir/.stage/$_name/spira/sentinel.sh"
tar -czf "$_dir/$_pat" -C "$_dir/.stage" "$_name" 2>/dev/null
rm -rf "$_dir/.stage"
GHEOF
chmod +x "$BIN/gh"

# Mock world.sh: records calls; exit codes configurable via env.
cat > "$BIN/world.sh" <<'WEOF'
#!/usr/bin/env bash
printf 'world %s\n' "$*" >> "${CALL_LOG:-/dev/null}"
case "${1:-}" in
    drain)  exit "${WORLD_DRAIN_EXIT:-0}" ;;
    resume) exit 0 ;;
    status) exit "${WORLD_STATUS_EXIT:-0}" ;;
    *)      exit 0 ;;
esac
WEOF
chmod +x "$BIN/world.sh"

# Mock activate.sh: creates release dir and swaps current symlink.
cat > "$BIN/activate.sh" <<'AEOF'
#!/usr/bin/env bash
printf 'activate %s\n' "$*" >> "${CALL_LOG:-/dev/null}"
[ "${ACTIVATE_EXIT:-0}" = "0" ] || exit "${ACTIVATE_EXIT}"
tarball="${*: -1}"
release_name="$(basename "$tarball" .tar.gz)"
releases="${SPIRA_RELEASES:?}"
mkdir -p "$releases/$release_name"
_tmp="$releases/.current.new.$$"
ln -s "$release_name" "$_tmp" && mv -T "$_tmp" "$releases/current"
AEOF
chmod +x "$BIN/activate.sh"

# Mock install.sh: records SPIRA_PROD value at call time.
cat > "$BIN/install.sh" <<'IEOF'
#!/usr/bin/env bash
printf 'install SPIRA_PROD=%s\n' "${SPIRA_PROD:-UNSET}" >> "${CALL_LOG:-/dev/null}"
exit "${INSTALL_EXIT:-0}"
IEOF
chmod +x "$BIN/install.sh"

# Mock cockpit layout.sh: no-op.
cat > "$BIN/layout.sh" <<'LEOF'
#!/usr/bin/env bash
printf 'layout %s\n' "$*" >> "${CALL_LOG:-/dev/null}"
exit 0
LEOF
chmod +x "$BIN/layout.sh"

# Mock doctor.sh: configurable exit.
cat > "$BIN/doctor.sh" <<'DEOF'
#!/usr/bin/env bash
printf 'doctor\n' >> "${CALL_LOG:-/dev/null}"
exit "${DOCTOR_EXIT:-0}"
DEOF
chmod +x "$BIN/doctor.sh"

# Mock skew.sh: configurable exit.
cat > "$BIN/skew.sh" <<'SEOF'
#!/usr/bin/env bash
printf 'skew %s\n' "$*" >> "${CALL_LOG:-/dev/null}"
exit "${SKEW_EXIT:-0}"
SEOF
chmod +x "$BIN/skew.sh"

# Mock systemctl: records calls; returns a dummy unit for list-units.
cat > "$BIN/systemctl" <<'SCEOF'
#!/usr/bin/env bash
printf 'SC %s\n' "$*" >> "${SC_LOG:-/dev/null}"
case "$*" in *list-units*) printf 'spira-sentinel-prod.service loaded active running\n' ;; esac
exit 0
SCEOF
chmod +x "$BIN/systemctl"

# ---------------------------------------------------------------------------
# run_deploy [env-pairs...] -- [deploy args...]
# ---------------------------------------------------------------------------
run_deploy() {
    local extra_env=() deploy_args=() in_args=0
    for _a in "$@"; do
        [ "$_a" = "--" ] && { in_args=1; continue; }
        [ "$in_args" = 1 ] && { deploy_args+=("$_a"); continue; }
        extra_env+=("$_a")
    done
    unset _a in_args
    > "$CALL_LOG" 2>/dev/null; > "$SC_LOG" 2>/dev/null
    env -i \
        "PATH=$BIN:$PATH" \
        "SPIRA_PATH=$BIN" \
        "HOME=$HOME" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_DB=/nonexistent-spira-db" \
        "SPIRA_RUN=$RUN_DIR" \
        "SPIRA_CONF=/nonexistent" \
        "SPIRA_RELEASES=$RELEASES" \
        "SPIRA_REPO=$FAKE_REPO" \
        "SPIRA_INSTANCE=prod" \
        "SPIRA_DOCTOR=1" \
        "SPIRA_SYSTEMCTL=$BIN/systemctl" \
        "SPIRA_WORLD_SH=$BIN/world.sh" \
        "SPIRA_ACTIVATE_SH=$BIN/activate.sh" \
        "SPIRA_INSTALL_SH=$BIN/install.sh" \
        "SPIRA_COCKPIT_LAYOUT_SH=$BIN/layout.sh" \
        "SPIRA_DOCTOR_SH=$BIN/doctor.sh" \
        "SPIRA_SKEW_SH=$BIN/skew.sh" \
        "CALL_LOG=$CALL_LOG" \
        "SC_LOG=$SC_LOG" \
        "GIT_CONFIG_NOSYSTEM=1" \
        "GIT_AUTHOR_NAME=test" \
        "GIT_AUTHOR_EMAIL=test@t" \
        "GIT_COMMITTER_NAME=test" \
        "GIT_COMMITTER_EMAIL=test@t" \
        "${extra_env[@]+"${extra_env[@]}"}" \
        bash "$DEPLOY" "${deploy_args[@]+"${deploy_args[@]}"}" 2>&1
}

# ==========================================================================
echo
echo "PROPERTY 2: basic deploy — latest resolves, gh fetches, current points at release"
# ==========================================================================
# FAIL-FIRST: without the tag, "latest" must fail.
_empty="$TMP/empty-repo"
git init -q "$_empty" 2>/dev/null || true
git -C "$_empty" -c user.email=t@t -c user.name=t \
    commit --allow-empty -q -m "init" 2>/dev/null || true
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
_out="$(run_deploy "SPIRA_REPO=$_empty" -- latest 2>&1)"
_rc=$?
not0 "fail-first: latest fails with no tags" "$_rc"
want "fail-first: mentions no tags"         "no spira-release-" "$_out"

# Happy path: deploy latest.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
_out="$(run_deploy -- latest 2>&1)"
_rc=$?
is0    "latest: exits 0"                "$_rc"
islink "latest: current -> $NEW_RELEASE" "$RELEASES/current" "$NEW_RELEASE"
want   "latest: gh download called"     "gh release download" "$(cat "$CALL_LOG")"
want   "latest: activate called"        "activate"            "$(cat "$CALL_LOG")"

# ==========================================================================
echo
echo "PROPERTY 3: already-current refuses without re-activating"
# ==========================================================================
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
mkdir -p "$RELEASES/$NEW_RELEASE"
ln -s "$NEW_RELEASE" "$RELEASES/current"

_out="$(run_deploy -- "$NEW_TAG" 2>&1)"
_rc=$?
not0    "already-current: exits non-zero" "$_rc"
want    "already-current: says already current" "already current" "$_out"
islink  "already-current: current unchanged" "$RELEASES/current" "$NEW_RELEASE"
notwant "already-current: gh not called"    "gh release download" "$(cat "$CALL_LOG")"

# ==========================================================================
echo
echo "PROPERTY 4: drain refuses — live aeons are not killed"
# ==========================================================================
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
mkdir -p "$RELEASES/$PRIOR_RELEASE"
ln -s "$PRIOR_RELEASE" "$RELEASES/current"

# FAIL-FIRST: verify the SC log detects a stop-aeon call if it happens.
printf 'SC --user stop spira-aeon-abc-prod.service\n' > "$SC_LOG"
if grep -q 'stop spira-aeon-' "$SC_LOG"; then
    ok "fail-first: SC log detects stop-aeon"
else
    bad "fail-first: SC log detects stop-aeon" "grep missed it"
fi
> "$SC_LOG"

_out="$(run_deploy "WORLD_DRAIN_EXIT=1" -- "$NEW_TAG" 2>&1)"
_rc=$?
not0    "drain-refuses: exits non-zero"           "$_rc"
want    "drain-refuses: mentions drain"           "drain" "$_out"
notwant "drain-refuses: no aeon stopped"          "stop spira-aeon-" "$(cat "$SC_LOG")"
islink  "drain-refuses: current unchanged"        "$RELEASES/current" "$PRIOR_RELEASE"

# ==========================================================================
echo
echo "PROPERTY 5: failed health check rolls current back to prior release"
# ==========================================================================
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
mkdir -p "$RELEASES/$PRIOR_RELEASE"
ln -s "$PRIOR_RELEASE" "$RELEASES/current"

# FAIL-FIRST: without health failure, current moves to new release.
_out="$(run_deploy -- "$NEW_TAG" 2>&1)"
if [ -L "$RELEASES/current" ] && [ "$(readlink "$RELEASES/current")" = "$NEW_RELEASE" ]; then
    ok "fail-first: healthy deploy moves current to new release"
else
    bad "fail-first: healthy deploy moves current to new release" \
        "current=$(readlink "$RELEASES/current" 2>/dev/null || echo '(missing)')"
fi

# With doctor failure: current must be restored to the prior release.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
mkdir -p "$RELEASES/$PRIOR_RELEASE"
ln -s "$PRIOR_RELEASE" "$RELEASES/current"

_out="$(run_deploy "DOCTOR_EXIT=1" -- "$NEW_TAG" 2>&1)"
_rc=$?
not0   "rollback: exits non-zero on health failure" "$_rc"
want   "rollback: mentions ROLLBACK"                "ROLLBACK" "$_out"
want   "rollback: names the failure"                "doctor"   "$_out"
islink "rollback: current restored to prior"        "$RELEASES/current" "$PRIOR_RELEASE"

# With skew failure: same behaviour.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
mkdir -p "$RELEASES/$PRIOR_RELEASE"
ln -s "$PRIOR_RELEASE" "$RELEASES/current"

_out="$(run_deploy "SKEW_EXIT=1" -- "$NEW_TAG" 2>&1)"
_rc=$?
not0   "rollback-skew: exits non-zero" "$_rc"
islink "rollback-skew: current restored to prior" "$RELEASES/current" "$PRIOR_RELEASE"

# ==========================================================================
echo
echo "PROPERTY 6: re-render calls install.sh with SPIRA_PROD=\$SPIRA_RELEASES/current"
# ==========================================================================
rm -rf "$RELEASES"; mkdir -p "$RELEASES"

_out="$(run_deploy -- "$NEW_TAG" 2>&1)"
_rc=$?
is0 "re-render: deploy exits 0" "$_rc"

if grep -q 'install SPIRA_PROD=' "$CALL_LOG" 2>/dev/null; then
    ok "re-render: install.sh was called"
else
    bad "re-render: install.sh was called" "not in call log"
fi

_got_prod="$(grep 'install SPIRA_PROD=' "$CALL_LOG" 2>/dev/null | head -1 | sed 's/^install SPIRA_PROD=//')"
if [ "$_got_prod" = "$RELEASES/current" ]; then
    ok "re-render: SPIRA_PROD=$RELEASES/current"
else
    bad "re-render: SPIRA_PROD=$RELEASES/current" "got [$_got_prod]"
fi

# ==========================================================================
echo
echo "PROPERTY 7: --dry-run touches nothing"
# ==========================================================================
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
> "$CALL_LOG"

# FAIL-FIRST: without --dry-run, current IS created.
_out="$(run_deploy -- "$NEW_TAG" 2>&1)"
if [ -L "$RELEASES/current" ]; then
    ok "fail-first: normal deploy creates current"
else
    bad "fail-first: normal deploy creates current" "current not created"
fi

# With --dry-run: nothing changes.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
> "$CALL_LOG"
_out="$(run_deploy -- --dry-run "$NEW_TAG" 2>&1)"
_rc=$?
is0     "dry-run: exits 0"              "$_rc"
want    "dry-run: says dry-run"         "dry-run"           "$_out"
if [ ! -e "$RELEASES/current" ]; then
    ok  "dry-run: current not created"
else
    bad "dry-run: current not created"  "current exists after --dry-run"
fi
notwant "dry-run: gh not called"        "gh release download" "$(cat "$CALL_LOG")"
notwant "dry-run: activate not called"  "activate"            "$(cat "$CALL_LOG")"

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
