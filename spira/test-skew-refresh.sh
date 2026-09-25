#!/usr/bin/env bash
#
# test-skew-refresh.sh — stage-and-swap refresh advances regardless of live aeon leases;
# running processes keep their old inode; dirty tracked files are stashed; gap reports
# commits behind with a positive control that verifies the ref is resolvable; a queue-mode
# repo's checkout is advanced by the landing pass's own refresh loop.
#
# covers: spira/skew.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-skew-refresh.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ── Fixture: a git remote with two commits, REPO left one behind ─────────────────────────
ORIGIN="$TMP/origin"
REPO="$TMP/repo"
git init -q "$ORIGIN"
git -C "$ORIGIN" config user.email "test@test"
git -C "$ORIGIN" config user.name "test"
mkdir -p "$ORIGIN/spira"
printf '#!/usr/bin/env bash\n# OLD\n' > "$ORIGIN/spira/lib.sh"
printf '# OLD_CANARY\n'               > "$ORIGIN/spira/canary.sh"
printf '# tracked\n'                  > "$ORIGIN/tracked.txt"
git -C "$ORIGIN" add spira/ tracked.txt
git -C "$ORIGIN" commit -q -m "base"
BASE_COMMIT="$(git -C "$ORIGIN" rev-parse HEAD)"

printf '#!/usr/bin/env bash\n# NEW\n' > "$ORIGIN/spira/lib.sh"
printf '# NEW_CANARY\n'               > "$ORIGIN/spira/canary.sh"
git -C "$ORIGIN" add spira/
git -C "$ORIGIN" commit -q -m "advance"
AHEAD_COMMIT="$(git -C "$ORIGIN" rev-parse HEAD)"

git clone -q "$ORIGIN" "$REPO"
git -C "$REPO" config user.email "test@test"
git -C "$REPO" config user.name "test"
git -C "$REPO" remote set-head origin --auto >/dev/null 2>&1 || true

reset_repo() { git -C "$REPO" reset -q --hard "$BASE_COMMIT"; }

# Stub Makefile in REPO so that `make -C "$REPO" install SPIRA_RELEASES=...` works
# without cargo.  Untracked — survives reset_repo but is never archived.
cat > "$REPO/Makefile" <<'STUBMAKE'
.PHONY: install
install:
	@set -eu; \
	_sha="$$(git rev-parse HEAD)"; \
	: "$${SPIRA_RELEASES?SPIRA_RELEASES not set}"; \
	mkdir -p "$$SPIRA_RELEASES/$$_sha"; \
	printf 'commit %%s\n' "$$_sha" > "$$SPIRA_RELEASES/$$_sha/MANIFEST"; \
	_tmp="$$SPIRA_RELEASES/.current.new.$$$$"; \
	ln -sf "$$_sha" "$$_tmp" && mv -T "$$_tmp" "$$SPIRA_RELEASES/current"; \
	printf 'stub-install: current -> %%s\n' "$$_sha"
STUBMAKE
reset_repo

run_skew_cmd() {
    local run_dir="$1"; shift
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$run_dir" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        bash "$HERE/skew.sh" "$@" 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# ===========================================================================
echo
echo "gap — POSITIVE CONTROL: HEAD behind origin/main must be reported:"
# ===========================================================================

RUN1="$(mktemp -d "$TMP/run-XXXXX")"
gap_behind_out="$(run_skew_cmd "$RUN1" gap "$REPO")"; gap_behind_rc=$?
is   "gap behind: exits 1"                    "1"                "$gap_behind_rc"
want "gap behind: reports commit count"       "commit(s) behind" "$gap_behind_out"
want "gap behind: names the base ref"         "origin/"          "$gap_behind_out"

# ===========================================================================
echo
echo "gap — at-base: HEAD at origin/main exits 0:"
# ===========================================================================

REMOTE_MAIN="$(git -C "$REPO" symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null)"
git -C "$REPO" merge --ff-only -q "$REMOTE_MAIN"
RUN2="$(mktemp -d "$TMP/run-XXXXX")"
gap_current_out="$(run_skew_cmd "$RUN2" gap "$REPO")"; gap_current_rc=$?
is   "gap current: exits 0"                   "0"                "$gap_current_rc"
want "gap current: reports 0 commits behind"  "0 commits behind" "$gap_current_out"
reset_repo

# ===========================================================================
echo
echo "refresh — POSITIVE CONTROL: live lease does NOT block advance:"
# ===========================================================================
# stage-and-swap is safe while aeons run — each file replaced atomically, old inodes kept.
# This test fails if the refusal approach is ever restored, which is the discriminating signal.

RUN3="$(mktemp -d "$TMP/run-XXXXX")"
mkdir -p "$RUN3/aeon"
FAKE_BEAD="sp-testaa"
printf '%s' "$(($(date +%s) + 3600))" > "$RUN3/aeon/$FAKE_BEAD.lease"
reset_repo

out3="$(run_skew_cmd "$RUN3" refresh "$REPO")"; rc3=$?
is   "lease held: exits 0"               "0"             "$rc3"
want "lease held: reports refreshed"     "refreshed"     "$out3"
HEAD3="$(git -C "$REPO" rev-parse HEAD)"
is   "lease held: checkout advanced"     "$AHEAD_COMMIT" "$HEAD3"

# ===========================================================================
echo
echo "refresh — inode preservation: held fd reads old content; path reads new:"
# ===========================================================================
# Open canary.sh before the refresh (simulating a bash session that sourced it). After the
# atomic mv the path points to a new inode; the held fd still references the old one.

reset_repo
exec 9< "$REPO/spira/canary.sh"
INODE_BEFORE="$(stat -c '%i' "$REPO/spira/canary.sh")"

RUN4="$(mktemp -d "$TMP/run-XXXXX")"
out4="$(run_skew_cmd "$RUN4" refresh "$REPO")"; rc4=$?
is   "inode test: refresh exits 0"         "0"    "$rc4"

INODE_AFTER="$(stat -c '%i' "$REPO/spira/canary.sh")"
[ "$INODE_BEFORE" != "$INODE_AFTER" ] \
    && ok "stage-and-swap created a new inode at the path" \
    || bad "stage-and-swap created a new inode at the path" "inode unchanged ($INODE_BEFORE)"

old_content="$(cat <&9)"; exec 9<&-
want   "held fd reads old content"  "OLD_CANARY" "$old_content"
nowant "held fd does not see new"   "NEW_CANARY" "$old_content"
want   "path now has new content"   "NEW_CANARY" "$(cat "$REPO/spira/canary.sh")"

# ===========================================================================
echo
echo "refresh — dirty tracked files are stashed, not refused:"
# ===========================================================================

reset_repo
printf '# machine-written\n' >> "$REPO/tracked.txt"

RUN5="$(mktemp -d "$TMP/run-XXXXX")"
out5="$(run_skew_cmd "$RUN5" refresh "$REPO")"; rc5=$?
is   "dirty: exits 0 (stashed and advanced)"   "0"         "$rc5"
want "dirty: reports stash"                    "stashed"   "$out5"
want "dirty: reports refreshed"               "refreshed" "$out5"
HEAD5="$(git -C "$REPO" rev-parse HEAD)"
is   "dirty: checkout advanced despite dirty"  "$AHEAD_COMMIT" "$HEAD5"
STASH_COUNT="$(git -C "$REPO" stash list 2>/dev/null | wc -l | tr -d ' ')"
[ "$STASH_COUNT" -ge 1 ] \
    && ok "dirty: stash entry created" \
    || bad "dirty: stash entry created" "stash list shows $STASH_COUNT entries"

# ===========================================================================
echo
echo "refresh — release mode: fetch + make install + symlink flip:"
# ===========================================================================
# In release mode (SPIRA_RELEASES set and releases/current is a symlink), refresh
# must: fast-forward the checkout, call make install, and flip releases/current.
# A mock 'make' creates the release dir and manifest so cargo is not required.

RELEASES="$TMP/releases"
OLD_SHA="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
mkdir -p "$RELEASES/$OLD_SHA"
ln -sf "$OLD_SHA" "$RELEASES/current"

reset_repo

run_skew_release() {
    local run_dir="$1"; shift
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$run_dir" \
        SPIRA_RELEASES="$RELEASES" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        bash "$HERE/skew.sh" "$@" 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# T1a: refresh in release mode when behind — must advance, call make install, flip symlink
reset_repo  # puts REPO at BASE_COMMIT, one behind AHEAD_COMMIT
RUN6="$(mktemp -d "$TMP/run-XXXXX")"
out6="$(run_skew_release "$RUN6" refresh "$REPO")"; rc6=$?
is   "release-mode refresh: exits 0"            "0"          "$rc6"
want "release-mode refresh: reports install"    "refreshed"  "$out6"
HEAD6="$(git -C "$REPO" rev-parse HEAD)"
is   "release-mode refresh: checkout advanced"  "$AHEAD_COMMIT" "$HEAD6"

NEW_CURRENT="$(readlink "$RELEASES/current")"
is   "release-mode refresh: current flipped to new sha" "$AHEAD_COMMIT" "$NEW_CURRENT"

# T1b: old release dir still present (not deleted)
[ -d "$RELEASES/$OLD_SHA" ] \
    && ok "release-mode refresh: old release dir preserved" \
    || bad "release-mode refresh: old release dir preserved" "missing $RELEASES/$OLD_SHA"

# T1c: new release dir was created with MANIFEST
[ -f "$RELEASES/$AHEAD_COMMIT/MANIFEST" ] \
    && ok "release-mode refresh: new release has MANIFEST" \
    || bad "release-mode refresh: new release has MANIFEST" "missing $RELEASES/$AHEAD_COMMIT/MANIFEST"

# T1d: refresh when already up-to-date — must exit 0 and skip make install
git -C "$REPO" merge --ff-only -q "$(git -C "$REPO" symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null)"
ln -sf "$AHEAD_COMMIT" "$RELEASES/current"  # already current

RUN7="$(mktemp -d "$TMP/run-XXXXX")"
out7="$(run_skew_release "$RUN7" refresh "$REPO")"; rc7=$?
is   "release-mode already-current: exits 0"        "0"       "$rc7"
want "release-mode already-current: reports current" "already" "$out7"
nowant "release-mode already-current: no make call"  "stub-install" "$out7"

# ===========================================================================
echo
echo "landing pass — a queue-mode repo's checkout is advanced by the refresh loop:"
# ===========================================================================
# verdict.sh advances a queue-mode repo's base by a fast-forward push; nothing else pulls
# the shared checkout, so it lags origin/<base> until landing.sh's own end-of-pass loop
# (landing.sh: "ADVANCE THE CHECKOUT HUMANS READ") calls skew.sh refresh on it — push and
# queue repos both, because those are the two modes that advance the base through Spira.
#
# MINIMUM LANDSTATE: one repo-map row with land=queue and no spira/* branches. land_repo
# reads the branch list first and returns immediately when it is empty (before touching
# bd, gate.sh or confine.sh), so reaching the refresh loop needs none of those — just
# landing.sh, lib.sh, conf.sh and the real skew.sh, wired through a repo-map whose only
# row is the queue repo itself, doubling as the home repo so nothing else is visited.
#
# THE POSITIVE CONTROL IS THE CONSTRUCTION. origin/main is advanced by one commit while
# the queue repo's checkout stays behind; a false-clean result — HEAD already at
# origin/main before the pass — is impossible because the extra commit is added
# explicitly, so silence means the refresh loop never fired.
QORIGIN="$TMP/qorigin.git"; QREPO="$TMP/qland-repo"; QSH="$TMP/qland-spira"; QRUN="$TMP/qland-run"
git init -q --bare -b main "$QORIGIN"
git init -q -b main "$QREPO"
git -C "$QREPO" config user.email "test@test"
git -C "$QREPO" config user.name "test"
git -C "$QREPO" commit -q --allow-empty -m "queue base"
git -C "$QREPO" remote add origin "$QORIGIN"
git -C "$QREPO" push -q origin main
git -C "$QREPO" fetch -q origin
git -C "$QREPO" remote set-head origin --auto >/dev/null 2>&1 || true

# Advance origin/main while QREPO's checkout stays behind.
_QCLONE="$(mktemp -d "$TMP/qclone-XXXXX")"
git clone -q "$QORIGIN" "$_QCLONE"
git -C "$_QCLONE" commit -q --allow-empty -m "origin advances"
git -C "$_QCLONE" push -q origin main
rm -rf "$_QCLONE"
git -C "$QREPO" fetch -q origin

QUEUE_NEW="$(git -C "$QREPO" rev-parse origin/main)"
[ "$(git -C "$QREPO" rev-parse HEAD)" != "$QUEUE_NEW" ] \
    || bad "queue-mode refresh setup" "checkout is already at origin/main before the pass"

mkdir -p "$QSH" "$QRUN"
cp "$HERE/landing.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/skew.sh" "$QSH/"
cat > "$QSH/repo-map" <<MAP
qfixture | $QREPO | queue | |
MAP

q_out="$(env -i PATH="$PATH" \
    HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$QSH" \
    SPIRA_RUN="$QRUN" \
    SPIRA_DB="$TMP/qland-no-db" \
    SPIRA_REPO="$QREPO" \
    SPIRA_HOME_REPO=qfixture \
    SPIRA_REPO_MAP="$QSH/repo-map" \
    SPIRA_DOLT_DATA="" \
    SPIRA_TESTDB_DATA="" \
    bash "$QSH/landing.sh" 2>&1)"

QUEUE_AFTER="$(git -C "$QREPO" rev-parse HEAD)"
[ "$QUEUE_AFTER" = "$QUEUE_NEW" ] \
    && ok  "a queue-mode repo's checkout is advanced to origin/main by the landing pass" \
    || bad "queue-mode refresh" "checkout at $(git -C "$QREPO" rev-parse --short HEAD), expected $(printf '%.7s' "$QUEUE_NEW")"
want "and the pass reports the refresh" "skew: refreshed to" "$q_out"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
