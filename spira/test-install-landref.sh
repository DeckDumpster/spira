#!/usr/bin/env bash
#
# test-install-landref.sh — install.sh refuses when the checkout is not on the landref
# or is behind it; SPIRA_INSTALL_FORCE=1 overrides the refusal.
#
# THREE PROPERTIES, each with a positive control (the fence MUST fire on the bad case) and
# a negative control (the fence MUST NOT fire when it should not):
#
#   1. WRONG BRANCH: install.sh exits non-zero and names the landref when HEAD is on a
#      branch other than the landref. SPIRA_INSTALL_FORCE=1 suppresses the refusal.
#
#   2. BEHIND LANDREF: install.sh exits non-zero and names the lag when the branch is
#      correct but the checkout is missing commits that are on the remote-tracking ref.
#      SPIRA_INSTALL_FORCE=1 suppresses the refusal.
#
#   3. CLEAN STATE: install.sh does NOT refuse when the checkout is on the landref
#      and fully up to date.
#
# EACH REFUSAL CASE ALSO ASSERTS that no unit file was written to the dest directory.
# The fence fires before any directory creation or file write, so a refusal must leave
# the box exactly as it was.
#
# THE FENCE IS TESTED IN ISOLATION by making watchd.sh a stub (exits 0, no output),
# so the only failure path that can fire before the fence is the path-collision check
# (skipped via SPIRA_CONF=/nonexistent). The fence fires; everything after it would need
# a full systemctl environment and is not what this suite covers.
#
# defect: sp-mlcd sp-y9zp
# tier: T1
# covers: systemd/install.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
nonzero()  { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }
dest_empty() {
    local name="$1" dest="$TMP/home/.config/systemd/user"
    local count; count=$(find "$dest" -maxdepth 1 \( -name '*.service' -o -name '*.timer' \) 2>/dev/null | wc -l)
    [ "$count" -eq 0 ] \
        && ok "$name" \
        || bad "$name" "$count file(s) written to dest on refusal path"
}

echo "test-install-landref.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture: a git repo that install.sh will run "from" (SPIRA_REPO), with
# origin/HEAD pointing to origin/main so spira_landref falls through to the
# git symbolic-ref path. A second commit is pushed to origin so there is
# something to be "behind".
# ---------------------------------------------------------------------------
ORIGIN="$TMP/origin.git"
REPO="$TMP/repo"
git init -q --bare -b main "$ORIGIN"
git init -q -b main "$REPO"
git -C "$REPO" config user.email t@t
git -C "$REPO" config user.name test
printf 'initial\n' > "$REPO/f"
git -C "$REPO" add f
git -C "$REPO" commit -qm "initial"
git -C "$REPO" remote add origin "$ORIGIN"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
# Cache origin/HEAD so spira_landref finds the base without a network call.
git -C "$REPO" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main

# A second commit on origin that the local checkout is missing (used for the BEHIND case).
CLONE="$TMP/clone"
git clone -q "$ORIGIN" "$CLONE" 2>/dev/null
git -C "$CLONE" config user.email t@t
git -C "$CLONE" config user.name test
printf 'extra\n' > "$CLONE/g"
git -C "$CLONE" add g
git -C "$CLONE" commit -qm "extra commit"
git -C "$CLONE" push -q origin main

# ---------------------------------------------------------------------------
# Minimal harness fixture. install.sh resolves its sibling scripts relative
# to $SRC (the systemd/ dir). conf.sh and lib.sh are symlinked so spira_landref
# is available. watchd.sh is stubbed: exits 0 with no output, so the watcher
# manifest loop has nothing to do and does not interfere.
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
mkdir -p "$FIXTURE/systemd" "$FIXTURE/spira"
ln -s "$HERE/../systemd/install.sh" "$FIXTURE/systemd/install.sh"
ln -s "$HERE/conf.sh"  "$FIXTURE/spira/conf.sh"
ln -s "$HERE/lib.sh"   "$FIXTURE/spira/lib.sh"

cat > "$FIXTURE/spira/watchd.sh" <<'WATCHD'
#!/usr/bin/env bash
# Stub: no watchers.
[ "${1:-}" = "units" ] && { echo ""; exit 0; }
exit 0
WATCHD
chmod +x "$FIXTURE/spira/watchd.sh"

# Mock systemctl: always reports no aeons, never active (so the live-aeon fence
# does not fire and we reach the end-state check, which also needs to be silent).
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$MOCK_BIN"
cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *list-units*aeon*) exit 0 ;;
    *daemon-reload*)   exit 0 ;;
    *is-active*)       printf 'active\n'; exit 0 ;;
    *enable*)          exit 0 ;;
    *restart*)         exit 0 ;;
    *start*)           exit 0 ;;
    *disable*)         exit 0 ;;
    *list-unit-files*) exit 0 ;;
    *list-units*)      exit 0 ;;
    *list-timers*)     exit 0 ;;
    *show*)            printf 'simple\n'; exit 0 ;;
    *) exit 0 ;;
esac
MOCK
chmod +x "$MOCK_BIN/systemctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/loginctl"; chmod +x "$MOCK_BIN/loginctl"

GIT_BIN="$(dirname "$(command -v git)")"

# inst <SPIRA_REPO> [env-overrides...] -- [install.sh-args]
# Run install.sh in a clean environment. SPIRA_INSTALL_FORCE defaults to empty.
inst() {
    local repo="$1"; shift
    local force=""
    while [[ "${1:-}" == *=* && "${1:-}" != "--" ]]; do
        case "$1" in
            SPIRA_INSTALL_FORCE=*) force="${1#SPIRA_INSTALL_FORCE=}" ;;
        esac
        shift
    done
    [ "${1:-}" = "--" ] && shift
    env -i \
        "PATH=$MOCK_BIN:$GIT_BIN:/usr/local/bin:/usr/bin:/bin" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_REPO=$repo" \
        SPIRA_REPO_MAP=/nonexistent \
        "SPIRA_HOME=$FIXTURE/spira" \
        "SPIRA_RUN=$TMP/run" \
        "SPIRA_DB=$TMP/db" \
        SPIRA_DOLT_DATA= \
        SPIRA_TESTDB_DATA= \
        "SPIRA_INSTALL_FORCE=$force" \
        bash "$FIXTURE/systemd/install.sh" "$@" 2>&1
}

mkdir -p "$TMP/home/.config/systemd/user" "$TMP/run"

# ===========================================================================
echo
echo "POSITIVE CONTROL — wrong branch: fence fires and cannot be silent."
# The wrong-branch case is planted explicitly; below we test the clean case.
# ===========================================================================

git -C "$REPO" checkout -qb feature/sp-test 2>/dev/null

out="$(inst "$REPO")"; rc=$?
nonzero   "wrong branch: exit non-zero"                                              "$rc"
want      "wrong branch: names the current branch in refusal"                        "feature/sp-test" "$out"
want      "wrong branch: names the landref"                                          "main" "$out"
want      "wrong branch: names the override"                                         "SPIRA_INSTALL_FORCE=1" "$out"
dest_empty "wrong branch: no unit file written to dest on refusal path"

# ===========================================================================
echo
echo "NEGATIVE CONTROL — SPIRA_INSTALL_FORCE=1 suppresses the wrong-branch refusal."
# ===========================================================================

out_force="$(inst "$REPO" SPIRA_INSTALL_FORCE=1)"; rc_force=$?
# The landref refuse line must not appear; the install may fail for other reasons
# (no unit templates in the minimal fixture), but that is not what we are testing here.
nowant "force on wrong branch: no landref refuse in output" "refusing — checkout is on branch" "$out_force"

# Restore to main.
git -C "$REPO" checkout -q main 2>/dev/null

# ===========================================================================
echo
echo "POSITIVE CONTROL — behind landref: fence fires."
# Push one commit directly to origin without pulling into REPO.
# ===========================================================================

git -C "$REPO" fetch -q origin  # updates origin/main tracking ref
behind_count="$(git -C "$REPO" rev-list --count "HEAD..origin/main" 2>/dev/null || echo 0)"
if [ "${behind_count:-0}" -lt 1 ]; then
    bad "behind setup" "REPO should be behind origin/main but behind_count=$behind_count"
else
    ok "behind setup: REPO is $behind_count commit(s) behind origin/main"
fi

out_behind="$(inst "$REPO")"; rc_behind=$?
nonzero   "behind: exit non-zero"                                                    "$rc_behind"
want      "behind: mentions being behind in refusal"                                 "behind" "$out_behind"
want      "behind: names the override"                                               "SPIRA_INSTALL_FORCE=1" "$out_behind"
dest_empty "behind: no unit file written to dest on refusal path"

# ===========================================================================
echo
echo "NEGATIVE CONTROL — SPIRA_INSTALL_FORCE=1 suppresses the behind refusal."
# ===========================================================================

out_behind_force="$(inst "$REPO" SPIRA_INSTALL_FORCE=1)"; rc_behind_force=$?
nowant "force when behind: no behind-refuse in output" "refusing — checkout is" "$out_behind_force"

# Pull to bring the checkout current.
git -C "$REPO" pull -q --rebase origin main 2>/dev/null

# ===========================================================================
echo
echo "CLEAN STATE — on landref, up to date: no refusal."
# ===========================================================================

out_clean="$(inst "$REPO")"; rc_clean=$?
nowant "clean state: no landref refuse in output" "refusing — checkout is on branch" "$out_clean"
nowant "clean state: no behind refuse in output"  "refusing — checkout is"           "$out_clean"

# ===========================================================================
echo
echo "ARTIFACT DEPLOY — SPIRA_REPO is not a git checkout: landref check is skipped."
# Positive control for the skip: we proved the refusal fires above (wrong-branch and
# behind cases), so silence here is meaningful.
# ===========================================================================

NOT_GIT="$TMP/not-a-git-repo"
mkdir -p "$NOT_GIT"

out_notgit="$(inst "$NOT_GIT")"; rc_notgit=$?
nowant "artifact deploy: no 'cannot resolve landref' in output" \
    "cannot resolve landref" "$out_notgit"
nowant "artifact deploy: no landref refusal in output" \
    "refusing — checkout is on branch" "$out_notgit"

# ===========================================================================
echo
echo "TAG INSTALL — detached HEAD at an exact tag: landref check is skipped."
# Positive control: the refusal fires for a branch checkout behind its base
# (proved above). Silence here is therefore meaningful.
# ===========================================================================

TAG_REPO="$TMP/tag-repo"
git init -q -b main "$TAG_REPO"
git -C "$TAG_REPO" config user.email t@t
git -C "$TAG_REPO" config user.name test
printf 'v1\n' > "$TAG_REPO/v"
git -C "$TAG_REPO" add v
git -C "$TAG_REPO" commit -qm "v1"
git -C "$TAG_REPO" tag v1.0.0
git -C "$TAG_REPO" checkout -q --detach v1.0.0

# No origin, no origin/HEAD — exactly what a shallow tag clone looks like.
out_tag="$(inst "$TAG_REPO")"; rc_tag=$?
nowant "tag install: no 'cannot resolve landref' in output"  "cannot resolve landref"    "$out_tag"
nowant "tag install: no landref refusal"                     "refusing — checkout is"    "$out_tag"
want   "tag install: mentions release install in output"     "release install from tag"  "$out_tag"
tl_summary
