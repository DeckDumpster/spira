#!/usr/bin/env bash
#
# test-deploy.sh — deploy.sh: fetch release, drain, activate, re-render, health check,
#                             rollback on failure, --dry-run, spira.conf update,
#                             draft-release refusal, migration check, bootstrap
#
# PROPERTIES
#   1. Self-check: deploy.sh must exist.
#   2. (a) Basic deploy: latest tag resolves; gh fetches tarball; current points at release.
#   3. (a) Already-current: refuses (exit 1) if the release is already active.
#   4. (b) Drain refuses: non-zero drain exit blocks deploy without killing aeons.
#   5. (c) Rollback non-first: failed health check restores the prior release.
#   6. (d) Unit re-render: install.sh is called with SPIRA_PROD=$SPIRA_RELEASES/current.
#   7. (e) Dry-run: --dry-run leaves the releases dir untouched.
#   8. (f) First-deploy rollback: no prior release → current removed, units on checkout, world resumed.
#   9. (g) spira.conf update: after successful deploy SPIRA_PROD written to conf.
#  10. (h) Draft release refused: latest skips a draft; a named draft is refused.
#  11. (i) Migration mismatch: refuses before drain when bd migrate schema fails.
#  12. (j) Bootstrap: deploy from tarball location with no deploy.sh in checkout.
#  16. (k) --force: slays live aeons (--keep-work --reopen --why "deploy <tag>") and proceeds.
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
DRAFT_RELEASE="spira-20260918T000000Z"
DRAFT_TAG="spira-release-$DRAFT_RELEASE"

# Fake git repo with release tag for "latest" resolution.
FAKE_REPO="$TMP/repo"
git init -q "$FAKE_REPO" 2>/dev/null || true
git -C "$FAKE_REPO" -c user.email=t@t -c user.name=t \
    commit --allow-empty -q -m "init" 2>/dev/null || true
git -C "$FAKE_REPO" tag "$NEW_TAG" 2>/dev/null || true

# Mock gh: handles release list, release view, and release download.
# GH_RELEASE_ASSET_NAME controls the asset returned by "release view --json assets".
# GH_RELEASE_VIEW overrides the entire "release view" response when set.
cat > "$BIN/gh" <<'GHEOF'
#!/usr/bin/env bash
printf 'gh %s\n' "$*" >> "${CALL_LOG:-/dev/null}"
[ "${GH_EXIT:-0}" = "0" ] || exit "${GH_EXIT}"
if [ "${1:-}" = release ] && [ "${2:-}" = list ]; then
    printf '%s\n' "${GH_RELEASE_LIST:-[]}"
    exit 0
fi
if [ "${1:-}" = release ] && [ "${2:-}" = view ]; then
    if [ -n "${GH_RELEASE_VIEW:-}" ]; then
        printf '%s\n' "$GH_RELEASE_VIEW"
        exit "${GH_VIEW_EXIT:-0}"
    fi
    # Dispatch on the --json fields requested.
    if printf '%s' "$*" | grep -q 'assets'; then
        _aname="${GH_RELEASE_ASSET_NAME:-}"
        if [ -n "$_aname" ]; then
            printf '{"assets":[{"name":"%s"}]}\n' "$_aname"
        else
            printf '{"assets":[]}\n'
        fi
    else
        printf '{"isDraft":false}\n'
    fi
    exit "${GH_VIEW_EXIT:-0}"
fi
# release download: create the tarball file in --dir.
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

# Mock slay.sh: records calls; configurable exit.
SLAY_LOG="$TMP/slay.log"
cat > "$BIN/slay.sh" <<'SLAYEOF'
#!/usr/bin/env bash
printf 'slay %s\n' "$*" >> "${SLAY_LOG:-/dev/null}"
exit "${SLAY_EXIT:-0}"
SLAYEOF
chmod +x "$BIN/slay.sh"

# Mock bd: records calls; fails for migrate schema when BD_MIGRATE_EXIT=1.
cat > "$BIN/bd" <<'BDEOF'
#!/usr/bin/env bash
printf 'bd %s\n' "$*" >> "${CALL_LOG:-/dev/null}"
case "$*" in
    *"migrate schema"*)
        if [ "${BD_MIGRATE_EXIT:-0}" != "0" ]; then
            printf 'database is at v61, binary knows up to v53\n'
            exit 1
        fi
        printf 'Schema already at v61\n'
        exit 0 ;;
esac
exit 0
BDEOF
chmod +x "$BIN/bd"

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
        "SPIRA_SLAY_SH=$BIN/slay.sh" \
        "CALL_LOG=$CALL_LOG" \
        "SC_LOG=$SC_LOG" \
        "GH_RELEASE_ASSET_NAME=$NEW_RELEASE.tar.gz" \
        "SLAY_LOG=$SLAY_LOG" \
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
want "fail-first: mentions no tags"         "spira-release-" "$_out"

# Happy path: deploy latest (gh release list returns [], falls back to git tags).
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
if [ "$_got_prod" = "$RELEASES/current/spira" ]; then
    ok "re-render: SPIRA_PROD=$RELEASES/current/spira"
else
    bad "re-render: SPIRA_PROD=$RELEASES/current/spira" "got [$_got_prod]"
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
echo "PROPERTY 8: first-deploy rollback — no prior release"
# (a) A failed first deploy ends with current removed, units restored, world resumed.
# ==========================================================================
# FAIL-FIRST: a successful first deploy creates current.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
_out="$(run_deploy "SPIRA_PROD=$TMP/fake-checkout" -- "$NEW_TAG" 2>&1)"
if [ -L "$RELEASES/current" ]; then
    ok "fail-first: successful first deploy creates current"
else
    bad "fail-first: successful first deploy creates current" "current missing"
fi

# Failed first deploy: health check fails, no prior release exists.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
_out="$(run_deploy "SPIRA_PROD=$TMP/fake-checkout" "DOCTOR_EXIT=1" -- "$NEW_TAG" 2>&1)"
_rc=$?
not0   "first-deploy-rollback: exits non-zero"                 "$_rc"
want   "first-deploy-rollback: mentions ROLLBACK"              "ROLLBACK" "$_out"
want   "first-deploy-rollback: notes no prior release"         "no prior release" "$_out"
if [ ! -e "$RELEASES/current" ]; then
    ok "first-deploy-rollback: current removed"
else
    bad "first-deploy-rollback: current removed" \
        "current still exists: $(readlink "$RELEASES/current" 2>/dev/null)"
fi
want   "first-deploy-rollback: world resumed"                  "world resume" "$(cat "$CALL_LOG")"
want   "first-deploy-rollback: install called with checkout"   "install SPIRA_PROD=$TMP/fake-checkout" \
       "$(cat "$CALL_LOG")"

# ==========================================================================
echo
echo "PROPERTY 9: spira.conf updated after successful deploy"
# (b) After a successful deploy SPIRA_PROD is written to spira.conf.
# ==========================================================================
# FAIL-FIRST: without a deploy, the conf file has no SPIRA_PROD.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
_conf_file="$TMP/test-deploy.conf"
: > "$_conf_file"
if ! grep -q 'SPIRA_PROD' "$_conf_file" 2>/dev/null; then
    ok "fail-first: empty conf has no SPIRA_PROD"
else
    bad "fail-first: empty conf has no SPIRA_PROD" "found unexpectedly"
fi

_out="$(run_deploy "SPIRA_CONF=$_conf_file" -- "$NEW_TAG" 2>&1)"
_rc=$?
is0 "conf-update: deploy exits 0" "$_rc"
_prod_in_conf="$(grep 'SPIRA_PROD' "$_conf_file" 2>/dev/null | tail -1)"
if printf '%s' "$_prod_in_conf" | grep -q "$RELEASES/current/spira"; then
    ok "conf-update: SPIRA_PROD written to conf pointing at releases/current/spira"
else
    bad "conf-update: SPIRA_PROD written to conf" "got [$_prod_in_conf]"
fi

# Existing SPIRA_PROD line is replaced, not appended.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
printf 'SPIRA_PROD = /old/checkout/spira\n' > "$_conf_file"
_out="$(run_deploy "SPIRA_CONF=$_conf_file" -- "$NEW_TAG" 2>&1)"
_count="$(grep -c 'SPIRA_PROD' "$_conf_file" 2>/dev/null || echo 0)"
[ "$_count" -eq 1 ] \
    && ok "conf-update: only one SPIRA_PROD line after update" \
    || bad "conf-update: only one SPIRA_PROD line after update" "found $_count"
notwant "conf-update: old path removed" "/old/checkout" "$(cat "$_conf_file")"

# ==========================================================================
echo
echo "PROPERTY 10: draft release refused"
# (c) latest skips a draft; a named draft is refused with a clear message.
# ==========================================================================
# FAIL-FIRST: with gh returning a published release, latest resolves.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
_pub_list="[{\"tagName\":\"$NEW_TAG\",\"isDraft\":false}]"
_out="$(run_deploy "GH_RELEASE_LIST=$_pub_list" -- latest 2>&1)"
_rc=$?
is0    "fail-first: published release resolves as latest" "$_rc"
islink "fail-first: current -> $NEW_RELEASE" "$RELEASES/current" "$NEW_RELEASE"

# latest skips a draft when a newer draft exists.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
_mixed_list="[{\"tagName\":\"$NEW_TAG\",\"isDraft\":false},{\"tagName\":\"$DRAFT_TAG\",\"isDraft\":true}]"
_out="$(run_deploy "GH_RELEASE_LIST=$_mixed_list" -- latest 2>&1)"
_rc=$?
is0    "draft-skip: latest exits 0"                 "$_rc"
islink "draft-skip: current -> non-draft release"   "$RELEASES/current" "$NEW_RELEASE"
notwant "draft-skip: draft release not activated"   "$DRAFT_RELEASE"    "$(readlink "$RELEASES/current" 2>/dev/null)"

# A named draft release is refused before any disruptive action.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
mkdir -p "$RELEASES/$PRIOR_RELEASE"
ln -s "$PRIOR_RELEASE" "$RELEASES/current"
_out="$(run_deploy "GH_RELEASE_VIEW={\"isDraft\":true}" -- "$DRAFT_TAG" 2>&1)"
_rc=$?
not0   "draft-named: exits non-zero"               "$_rc"
want   "draft-named: mentions draft"               "draft" "$_out"
islink "draft-named: current unchanged"            "$RELEASES/current" "$PRIOR_RELEASE"
notwant "draft-named: drain not called"            "world drain" "$(cat "$CALL_LOG")"

# ==========================================================================
echo
echo "PROPERTY 11: DB migration mismatch refuses before drain"
# (e) A bd migration mismatch stops the deploy before world.sh drain.
# ==========================================================================
# Set up a fake DB directory so the migration check runs.
_testdb="$TMP/testdb"
mkdir -p "$_testdb/.beads"

# FAIL-FIRST: when bd migrate schema succeeds, drain IS called.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
_out="$(run_deploy \
    "SPIRA_DB=$_testdb" "SPIRA_BD=$BIN/bd" \
    -- "$NEW_TAG" 2>&1)"
_rc=$?
is0  "fail-first: healthy migration allows deploy" "$_rc"
want "fail-first: drain called when migration ok"  "world drain" "$(cat "$CALL_LOG")"

# Migration mismatch: refuses, names the failure, does not reach drain.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
_out="$(run_deploy \
    "SPIRA_DB=$_testdb" "SPIRA_BD=$BIN/bd" "BD_MIGRATE_EXIT=1" \
    -- "$NEW_TAG" 2>&1)"
_rc=$?
not0    "migration-check: exits non-zero"          "$_rc"
want    "migration-check: mentions mismatch"        "migration mismatch" "$_out"
notwant "migration-check: drain not called"         "world drain"       "$(cat "$CALL_LOG")"
notwant "migration-check: activate not called"      "activate"          "$(cat "$CALL_LOG")"

# ==========================================================================
echo
echo "PROPERTY 13: asset stamp differs from tag stamp — asset name is authoritative"
# (a) When the release tarball stamp differs from the tag stamp, deploy uses the asset
# name (from gh release view --json assets) for the release directory, not the tag.
# FAIL-FIRST: with no assets returned, deploy refuses.
# ==========================================================================
DIFF_RELEASE="spira-20260917T999999Z"
DIFF_TAG="spira-release-$DIFF_RELEASE"

rm -rf "$RELEASES"; mkdir -p "$RELEASES"
_out="$(run_deploy "GH_RELEASE_ASSET_NAME=" -- "$DIFF_TAG" 2>&1)"
_rc=$?
not0 "asset-mismatch/fail-first: exits non-zero when no asset" "$_rc"
want "asset-mismatch/fail-first: mentions asset" "asset" "$_out"

# Happy path: tag stamp (from DIFF_TAG) differs from asset stamp ($NEW_RELEASE).
# Deploy must create current -> $NEW_RELEASE (from the asset), not DIFF_RELEASE (from tag).
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
git -C "$FAKE_REPO" tag "$DIFF_TAG" 2>/dev/null || true
_out="$(run_deploy "GH_RELEASE_ASSET_NAME=$NEW_RELEASE.tar.gz" -- "$DIFF_TAG" 2>&1)"
_rc=$?
is0    "asset-mismatch: exits 0"                             "$_rc"
islink "asset-mismatch: current -> asset release, not tag"   "$RELEASES/current" "$NEW_RELEASE"
notwant "asset-mismatch: tag-derived name not used"          "$DIFF_RELEASE" \
        "$(readlink "$RELEASES/current" 2>/dev/null)"

# ==========================================================================
echo
echo "PROPERTY 14: release-mode bootstrap — no current symlink, SPIRA_PROD inside SPIRA_RELEASES"
# (c) An instance whose spira.conf sets SPIRA_PROD to a release dir directly (no current
# symlink) gets current created as prev_release so rollback works on failure.
# ==========================================================================
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
mkdir -p "$RELEASES/$PRIOR_RELEASE"

# FAIL-FIRST: without the bootstrap the rollback would have no prev_release.
# Verify the doctor-failure path actually restores current when prev_release is set.
_out="$(run_deploy "SPIRA_PROD=$RELEASES/$PRIOR_RELEASE/spira" "DOCTOR_EXIT=1" \
    -- "$NEW_TAG" 2>&1)"
_rc=$?
not0   "bootstrap-rollback: exits non-zero on health failure"      "$_rc"
want   "bootstrap-rollback: mentions ROLLBACK"                     "ROLLBACK" "$_out"
islink "bootstrap-rollback: current restored to bootstrap release" \
    "$RELEASES/current" "$PRIOR_RELEASE"

# Without health failure: current ends at the new release, prev_release was the old one.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
mkdir -p "$RELEASES/$PRIOR_RELEASE"
_out="$(run_deploy "SPIRA_PROD=$RELEASES/$PRIOR_RELEASE/spira" -- "$NEW_TAG" 2>&1)"
_rc=$?
is0    "bootstrap-ok: exits 0"                             "$_rc"
islink "bootstrap-ok: current -> new release"              "$RELEASES/current" "$NEW_RELEASE"

# ==========================================================================
echo
echo "PROPERTY 15: --dry-run ExecStart check"
# (d) --dry-run resolves the asset and checks ExecStart target executability
# when a current release is active. Fails if target is not executable.
# ==========================================================================
# FAIL-FIRST: dry-run with a non-executable ExecStart target exits non-zero and names ExecStart.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
mkdir -p "$RELEASES/$NEW_RELEASE"        # release dir exists but no sentinel.sh
ln -s "$NEW_RELEASE" "$RELEASES/current"
_out="$(run_deploy -- --dry-run "$NEW_TAG" 2>&1)"
_rc=$?
not0 "dry-run/bad-exec: exits non-zero"    "$_rc"
want "dry-run/bad-exec: mentions ExecStart" "ExecStart" "$_out"

# Happy path: with an executable sentinel.sh in place, dry-run exits 0 and says dry-run.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
mkdir -p "$RELEASES/$NEW_RELEASE/spira"
printf '#!/bin/sh\n' > "$RELEASES/$NEW_RELEASE/spira/sentinel.sh"
chmod +x "$RELEASES/$NEW_RELEASE/spira/sentinel.sh"
ln -s "$NEW_RELEASE" "$RELEASES/current"
> "$CALL_LOG"
_out="$(run_deploy -- --dry-run "$NEW_TAG" 2>&1)"
_rc=$?
is0  "dry-run/good-exec: exits 0"       "$_rc"
want "dry-run/good-exec: says dry-run"  "dry-run" "$_out"
want "dry-run/good-exec: mentions ExecStart" "ExecStart" "$_out"
notwant "dry-run/good-exec: activate not called" "activate" "$(cat "$CALL_LOG")"

# ==========================================================================
echo
echo "PROPERTY 12: bootstrap — deploy.sh works with no deploy.sh in the checkout"
# (d) deploy.sh does not require the checkout (SPIRA_REPO) to carry a current copy.
# ==========================================================================
# Verify the fake repo has no deploy.sh (it is a bare git init with no files tracked).
if [ ! -f "$FAKE_REPO/spira/deploy.sh" ]; then
    ok "bootstrap: FAKE_REPO has no spira/deploy.sh"
else
    bad "bootstrap: FAKE_REPO should not have deploy.sh" "found at $FAKE_REPO/spira/deploy.sh"
fi

# Deploy still succeeds when run from the harness location (simulating a tarball).
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
_out="$(run_deploy -- "$NEW_TAG" 2>&1)"
_rc=$?
is0    "bootstrap: deploy exits 0 with no deploy.sh in checkout" "$_rc"
islink "bootstrap: current -> $NEW_RELEASE" "$RELEASES/current" "$NEW_RELEASE"

# ==========================================================================
echo
echo "PROPERTY 16: --force slays live aeons and proceeds with deploy"
# ==========================================================================
FORCE_BEAD="sp-frce1"

# FAIL-FIRST: without --force, a drain refusal blocks the deploy.
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
> "$SLAY_LOG"
_out="$(run_deploy "WORLD_DRAIN_EXIT=1" -- "$NEW_TAG" 2>&1)"
_rc=$?
not0    "fail-first: drain refusal blocks without --force"  "$_rc"
notwant "fail-first: slay not called without --force"       "slay --bead" "$(cat "$SLAY_LOG")"

# Simulate a live aeon: pidfile pointing at the running test shell (process exists in /proc).
_force_pf="$RUN_DIR/aeon-bahamut-$FORCE_BEAD.pid"
printf '%s\n' "$$" > "$_force_pf"

rm -rf "$RELEASES"; mkdir -p "$RELEASES"
> "$CALL_LOG"; > "$SLAY_LOG"
_out="$(run_deploy -- --force "$NEW_TAG" 2>&1)"
_rc=$?
is0     "--force: deploy exits 0"                                "$_rc"
islink  "--force: current -> $NEW_RELEASE"                       "$RELEASES/current" "$NEW_RELEASE"
want    "--force: slay called"                                   "slay" "$(cat "$SLAY_LOG")"
want    "--force: slay given --bead $FORCE_BEAD"                 "--bead $FORCE_BEAD" "$(cat "$SLAY_LOG")"
want    "--force: slay given --keep-work"                        "--keep-work" "$(cat "$SLAY_LOG")"
want    "--force: slay given --reopen"                           "--reopen" "$(cat "$SLAY_LOG")"
want    "--force: slay given --why with tag"                     "--why deploy $NEW_TAG" "$(cat "$SLAY_LOG")"
want    "--force: world drain called"                            "world drain" "$(cat "$CALL_LOG")"

rm -f "$_force_pf"

# ==========================================================================
echo
echo "PROPERTY 17: read-only release dir — sidecar written to .tags/, deploy succeeds"
# Acceptance test for issue #109: activate.sh makes the release dir read-only, so
# the old deploy.sh silently failed to write .tag inside it, skew fell back to
# timestamps, and every deploy rolled back.
#
# FAIL-FIRST: with activate.sh that chmod's the dir and a skew that reads .tag
# from the OLD location (inside the release dir), the sidecar is missing, skew
# gets CANNOT-VERIFY or a timestamp mismatch, and the test records this.
# ==========================================================================

# Release with intentionally mismatched timestamps: asset stem uses a NEWER time,
# tag uses an OLDER time. The old timestamp fallback would see them differ → NOT-LATEST.
RDONLY_TS_TAG="20260913T144935Z"      # tag timestamp (older)
RDONLY_TS_ASSET="20260913T145022Z"    # asset/tarball timestamp (newer, from issue #51)
RDONLY_RELEASE="spira-${RDONLY_TS_ASSET}"
RDONLY_TAG="spira-release-spira-${RDONLY_TS_TAG}"

# Set up a git repo with exactly one tag, pointing at a commit.
RDONLY_REPO="$TMP/rdonly-repo"
git init -q "$RDONLY_REPO"
git -C "$RDONLY_REPO" config user.email "t@t"
git -C "$RDONLY_REPO" config user.name "t"
git -C "$RDONLY_REPO" commit --allow-empty -q -m "init"
RDONLY_COMMIT="$(git -C "$RDONLY_REPO" rev-parse HEAD)"
git -C "$RDONLY_REPO" tag -a "$RDONLY_TAG" HEAD \
    -m "$(printf 'spira release\nbead: sp-test')"

# activate.sh mock: creates release dir, chmod's it read-only, writes MANIFEST.
# Crucially it does NOT create a .tag inside the release dir (the real one can't).
RDONLY_ACTIVATE="$TMP/rdonly-activate.sh"
cat > "$RDONLY_ACTIVATE" <<RAEOF
#!/usr/bin/env bash
printf 'activate %s\n' "\$*" >> "\${CALL_LOG:-/dev/null}"
[ "\${ACTIVATE_EXIT:-0}" = "0" ] || exit "\${ACTIVATE_EXIT}"
tarball="\${*: -1}"
release_name="\$(basename "\$tarball" .tar.gz)"
releases="\${SPIRA_RELEASES:?}"
mkdir -p "\$releases/\$release_name"
printf 'commit %s\ntimestamp %s\n' "\${RDONLY_COMMIT}" "\${RDONLY_TS_ASSET}" \
    > "\$releases/\$release_name/MANIFEST"
chmod -R a-w "\$releases/\$release_name"
_tmp="\$releases/.current.new.\$\$"
ln -s "\$release_name" "\$_tmp" && mv -T "\$_tmp" "\$releases/current"
RAEOF
chmod +x "$RDONLY_ACTIVATE"

# FAIL-FIRST against old code: use a skew stub that reads from the OLD sidecar
# location. Since the dir is read-only, the sidecar was never written there.
# Skew falls back to timestamps: RDONLY_TS_ASSET != RDONLY_TS_TAG → NOT-LATEST.
RDONLY_BIN_OLD="$TMP/rdonly-bin-old"
mkdir -p "$RDONLY_BIN_OLD"
# Copy standard mocks from BIN.
for _f in gh world.sh install.sh layout.sh doctor.sh slay.sh bd systemctl; do
    cp "$BIN/$_f" "$RDONLY_BIN_OLD/$_f" 2>/dev/null || true
done
# skew stub that reads from OLD location and uses timestamp fallback.
cat > "$RDONLY_BIN_OLD/skew.sh" <<SKEWOLDEOF
#!/usr/bin/env bash
printf 'skew %s\n' "\$*" >> "\${CALL_LOG:-/dev/null}"
case "\${1:-}" in check)
    current_link="\${SPIRA_RELEASES}/current"
    activated="\$(readlink "\$current_link" 2>/dev/null)" || exit 3
    sidecar="\${SPIRA_RELEASES}/\${activated}/.tag"
    if [ -f "\$sidecar" ]; then
        release_tag="\$(tr -d '\n' < "\$sidecar")"
    else
        release_tag=""
    fi
    latest_tag="\$(git -C "\$SPIRA_REPO" tag -l 'spira-release-*' 2>/dev/null | sort | tail -1)"
    if [ -n "\$release_tag" ]; then
        [ "\$release_tag" = "\$latest_tag" ] || { echo "NOT-LATEST"; exit 1; }
    else
        activated_ts="\${activated#spira-}"
        latest_ts="\${latest_tag##*-}"
        [ "\$activated_ts" = "\$latest_ts" ] || { echo "NOT-LATEST (ts fallback)"; exit 1; }
    fi
    echo "skew: in effect"
    exit 0
    ;;
esac
exit 0
SKEWOLDEOF
chmod +x "$RDONLY_BIN_OLD/skew.sh"

rm -rf "$RELEASES"; mkdir -p "$RELEASES"
> "$CALL_LOG"
_ff_out="$(env -i \
    "PATH=$RDONLY_BIN_OLD:$PATH" \
    "SPIRA_PATH=$RDONLY_BIN_OLD" \
    "HOME=$HOME" \
    "SPIRA_HOME=$HERE" \
    "SPIRA_DB=/nonexistent-spira-db" \
    "SPIRA_RUN=$RUN_DIR" \
    "SPIRA_CONF=/nonexistent" \
    "SPIRA_RELEASES=$RELEASES" \
    "SPIRA_REPO=$RDONLY_REPO" \
    "SPIRA_INSTANCE=prod" \
    "SPIRA_DOCTOR=1" \
    "SPIRA_SYSTEMCTL=$BIN/systemctl" \
    "SPIRA_WORLD_SH=$BIN/world.sh" \
    "SPIRA_ACTIVATE_SH=$RDONLY_ACTIVATE" \
    "SPIRA_INSTALL_SH=$BIN/install.sh" \
    "SPIRA_COCKPIT_LAYOUT_SH=$BIN/layout.sh" \
    "SPIRA_DOCTOR_SH=$BIN/doctor.sh" \
    "SPIRA_SKEW_SH=$RDONLY_BIN_OLD/skew.sh" \
    "SPIRA_SLAY_SH=$BIN/slay.sh" \
    "CALL_LOG=$CALL_LOG" \
    "SC_LOG=$SC_LOG" \
    "RDONLY_COMMIT=$RDONLY_COMMIT" \
    "RDONLY_TS_ASSET=$RDONLY_TS_ASSET" \
    "GH_RELEASE_ASSET_NAME=$RDONLY_RELEASE.tar.gz" \
    "SLAY_LOG=$SLAY_LOG" \
    "GIT_CONFIG_NOSYSTEM=1" \
    "GIT_AUTHOR_NAME=test" \
    "GIT_AUTHOR_EMAIL=test@t" \
    "GIT_COMMITTER_NAME=test" \
    "GIT_COMMITTER_EMAIL=test@t" \
    bash "$DEPLOY" "$RDONLY_TAG" 2>&1)"
_ff_rc=$?
not0 "rdonly/fail-first: old sidecar location → NOT-LATEST → rollback (exits non-zero)" "$_ff_rc"
want "rdonly/fail-first: rollback mentioned"   "ROLLBACK" "$_ff_out"
want "rdonly/fail-first: skew triggered rollback" "skew" "$_ff_out"

# HAPPY PATH: same setup, but deploy.sh writes sidecar to .tags/ (the fix).
rm -rf "$RELEASES"; mkdir -p "$RELEASES"
> "$CALL_LOG"
_rdonly_out="$(env -i \
    "PATH=$BIN:$PATH" \
    "SPIRA_PATH=$BIN" \
    "HOME=$HOME" \
    "SPIRA_HOME=$HERE" \
    "SPIRA_DB=/nonexistent-spira-db" \
    "SPIRA_RUN=$RUN_DIR" \
    "SPIRA_CONF=/nonexistent" \
    "SPIRA_RELEASES=$RELEASES" \
    "SPIRA_REPO=$RDONLY_REPO" \
    "SPIRA_INSTANCE=prod" \
    "SPIRA_DOCTOR=1" \
    "SPIRA_SYSTEMCTL=$BIN/systemctl" \
    "SPIRA_WORLD_SH=$BIN/world.sh" \
    "SPIRA_ACTIVATE_SH=$RDONLY_ACTIVATE" \
    "SPIRA_INSTALL_SH=$BIN/install.sh" \
    "SPIRA_COCKPIT_LAYOUT_SH=$BIN/layout.sh" \
    "SPIRA_DOCTOR_SH=$BIN/doctor.sh" \
    "SPIRA_SKEW_SH=$HERE/skew.sh" \
    "SPIRA_SLAY_SH=$BIN/slay.sh" \
    "CALL_LOG=$CALL_LOG" \
    "SC_LOG=$SC_LOG" \
    "RDONLY_COMMIT=$RDONLY_COMMIT" \
    "RDONLY_TS_ASSET=$RDONLY_TS_ASSET" \
    "GH_RELEASE_ASSET_NAME=$RDONLY_RELEASE.tar.gz" \
    "SLAY_LOG=$SLAY_LOG" \
    "GIT_CONFIG_NOSYSTEM=1" \
    "GIT_AUTHOR_NAME=test" \
    "GIT_AUTHOR_EMAIL=test@t" \
    "GIT_COMMITTER_NAME=test" \
    "GIT_COMMITTER_EMAIL=test@t" \
    bash "$DEPLOY" "$RDONLY_TAG" 2>&1)"
_rdonly_rc=$?
is0    "rdonly: deploy exits 0 (no rollback)"        "$_rdonly_rc"
notwant "rdonly: no ROLLBACK"                        "ROLLBACK" "$_rdonly_out"
islink "rdonly: current -> $RDONLY_RELEASE"          "$RELEASES/current" "$RDONLY_RELEASE"
if [ -f "$RELEASES/.tags/$RDONLY_RELEASE" ]; then
    ok "rdonly: sidecar written to .tags/$RDONLY_RELEASE"
else
    bad "rdonly: sidecar written to .tags/$RDONLY_RELEASE" "file missing"
fi
_sidecar_val="$(cat "$RELEASES/.tags/$RDONLY_RELEASE" 2>/dev/null | tr -d '\n')"
is "rdonly: sidecar contains correct tag" "$RDONLY_TAG" "$_sidecar_val"
if [ ! -w "$RELEASES/$RDONLY_RELEASE" ]; then
    ok "rdonly: release dir is read-only (confirms chmod worked)"
else
    bad "rdonly: release dir is read-only" "dir is still writable"
fi

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
