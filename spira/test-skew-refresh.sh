#!/usr/bin/env bash
#
# test-skew-refresh.sh — skew.sh refresh refuses while a live aeon lease is held, stashes
# dirty tracked files rather than refusing, and gap reports commits behind with a positive
# control that verifies the ref is resolvable.
#
# covers: spira/skew.sh
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

# ---------------------------------------------------------------------------
# ORIGIN: a bare git repo acting as the remote.
# REPO: a clone of ORIGIN, left one commit behind HEAD, simulating a real
# running checkout that has not yet been advanced.
# ---------------------------------------------------------------------------
ORIGIN="$TMP/origin"
REPO="$TMP/repo"
git init -q "$ORIGIN"
git -C "$ORIGIN" config user.email "test@test"
git -C "$ORIGIN" config user.name "test"
mkdir -p "$ORIGIN/spira"
printf '#!/usr/bin/env bash\n' > "$ORIGIN/spira/lib.sh"
printf '# tracked file\n'     > "$ORIGIN/tracked.txt"
git -C "$ORIGIN" add spira/ tracked.txt
git -C "$ORIGIN" commit -q -m "base"
BASE_COMMIT="$(git -C "$ORIGIN" rev-parse HEAD)"

printf '# v2\n' >> "$ORIGIN/spira/lib.sh"
git -C "$ORIGIN" add spira/lib.sh
git -C "$ORIGIN" commit -q -m "advance"
AHEAD_COMMIT="$(git -C "$ORIGIN" rev-parse HEAD)"

git clone -q "$ORIGIN" "$REPO"
git -C "$REPO" config user.email "test@test"
git -C "$REPO" config user.name "test"
# Detect the actual remote default branch (master vs main depends on git config).
git -C "$REPO" remote set-head origin --auto >/dev/null 2>&1 || true
REMOTE_MAIN="$(git -C "$REPO" symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null)"
# Leave REPO one commit behind.
git -C "$REPO" reset -q --hard "$BASE_COMMIT"

# run_skew_cmd <run_dir> <subcommand> [args...]
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

git -C "$REPO" merge --ff-only -q "$REMOTE_MAIN"
RUN2="$(mktemp -d "$TMP/run-XXXXX")"
gap_current_out="$(run_skew_cmd "$RUN2" gap "$REPO")"; gap_current_rc=$?
is   "gap current: exits 0"                   "0"                "$gap_current_rc"
want "gap current: reports 0 commits behind"  "0 commits behind" "$gap_current_out"
# Reset for remaining tests.
git -C "$REPO" reset -q --hard "$BASE_COMMIT"

# ===========================================================================
echo
echo "gap — cannot-check: exit 3 when repo has no git checkout:"
# ===========================================================================

RUN3="$(mktemp -d "$TMP/run-XXXXX")"
gap_notgit_out="$(run_skew_cmd "$RUN3" gap "$TMP/not-a-repo" 2>&1)"; gap_notgit_rc=$?
is "gap not-a-repo: exits 3" "3" "$gap_notgit_rc"

# ===========================================================================
echo
echo "refresh — POSITIVE CONTROL: live lease blocks advance:"
# ===========================================================================

RUN4="$(mktemp -d "$TMP/run-XXXXX")"
mkdir -p "$RUN4/aeon"
FAKE_BEAD="sp-test99"
# Deadline far in the future.
printf '%s' "$(($(date +%s) + 3600))" > "$RUN4/aeon/$FAKE_BEAD.lease"

git -C "$REPO" reset -q --hard "$BASE_COMMIT"
ref_blocked_out="$(run_skew_cmd "$RUN4" refresh "$REPO")"; ref_blocked_rc=$?
is   "refresh lease-blocked: exits 1"                "1"          "$ref_blocked_rc"
want "refresh lease-blocked: names the holder"       "$FAKE_BEAD" "$ref_blocked_out"
want "refresh lease-blocked: says live lease"        "live lease" "$ref_blocked_out"
# Verify the checkout did NOT advance.
HEAD_AFTER="$(git -C "$REPO" rev-parse HEAD)"
is   "refresh lease-blocked: checkout stayed put"    "$BASE_COMMIT" "$HEAD_AFTER"

# ===========================================================================
echo
echo "refresh — expired lease does not block:"
# ===========================================================================

RUN5="$(mktemp -d "$TMP/run-XXXXX")"
mkdir -p "$RUN5/aeon"
# Deadline in the past.
printf '%s' "$(($(date +%s) - 1))" > "$RUN5/aeon/sp-expired.lease"

git -C "$REPO" reset -q --hard "$BASE_COMMIT"
ref_expired_out="$(run_skew_cmd "$RUN5" refresh "$REPO")"; ref_expired_rc=$?
is   "refresh expired-lease: exits 0"           "0"          "$ref_expired_rc"
want "refresh expired-lease: reports refreshed" "refreshed"  "$ref_expired_out"
HEAD_AFTER2="$(git -C "$REPO" rev-parse HEAD)"
is   "refresh expired-lease: checkout advanced" "$AHEAD_COMMIT" "$HEAD_AFTER2"

# ===========================================================================
echo
echo "refresh — dirty tracked files are stashed, not refused:"
# ===========================================================================

git -C "$REPO" reset -q --hard "$BASE_COMMIT"
# Modify a tracked file to create dirty state.
printf '# machine-written quarantine\n' >> "$REPO/tracked.txt"

RUN6="$(mktemp -d "$TMP/run-XXXXX")"
ref_dirty_out="$(run_skew_cmd "$RUN6" refresh "$REPO")"; ref_dirty_rc=$?
is   "refresh dirty: exits 0 (stashed and advanced)"   "0"         "$ref_dirty_rc"
want "refresh dirty: reports stash"                    "stashed"   "$ref_dirty_out"
want "refresh dirty: reports refreshed"                "refreshed" "$ref_dirty_out"
HEAD_AFTER3="$(git -C "$REPO" rev-parse HEAD)"
is   "refresh dirty: checkout advanced despite dirty"  "$AHEAD_COMMIT" "$HEAD_AFTER3"
# Dirty state is in the stash, not lost.
STASH_COUNT="$(git -C "$REPO" stash list 2>/dev/null | wc -l | tr -d ' ')"
[ "$STASH_COUNT" -ge 1 ] \
    && ok "refresh dirty: stash entry created" \
    || bad "refresh dirty: stash entry created" "stash list shows $STASH_COUNT entries"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
