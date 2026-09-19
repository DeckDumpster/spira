#!/usr/bin/env bash
#
# test-skew-refresh.sh — stage-and-swap refresh advances regardless of live aeon leases;
# running processes keep their old inode; dirty tracked files are stashed; gap reports
# commits behind with a positive control that verifies the ref is resolvable.
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

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
