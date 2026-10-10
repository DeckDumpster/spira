#!/usr/bin/env bash
#
# test-worktree-branch-mismatch.sh — spira_destroy_worktree refuses when the WORKTREE'S
# OWN directory names a bead with a live aeon, even though the caller derived a different,
# unheld id from whatever branch happens to be checked out there.
#
#   ./test-worktree-branch-mismatch.sh
#
# THE BUG (sp-87csm). A worktree path is keyed on the bead (its directory name), but a
# child bead created sharing its parent's branch label leaves a worktree directory named
# for the child while the branch checked out inside it names the parent. sending.sh's
# landing sweep parses the id from the LANDED BRANCH, not from the path, and asked
# spira_holder_witnesses only about that branch's bead. Once the parent bead had no aeon
# left (it was already closed), the witness said "nobody home" and the child's own worktree
# — live, mid-session, holding uncommitted work — was salvaged and removed out from under
# it. Confirmed against the production reap log: sp-eq8a4 (parent, closed) reaped
# .../worktree/sp-eq8a4.2 (child, in_progress) at the exact minute the session's uncommitted
# work vanished.
#
# THE FIX. spira_destroy_worktree now also checks spira_holder_witnesses under the id taken
# from the worktree path itself (basename), refusing if either name is held.
#
# THE PROPERTIES UNDER TEST.
#   1. POSITIVE CONTROL: the mismatch is refused while the path's own bead has a live aeon,
#      even though the caller's (branch-derived) id has none.
#   2. Once that aeon is gone, the same call proceeds — the fix must not become a permanent
#      refusal once nobody is actually home.
#   3. THE ORDINARY CASE (id == path's own name, as every non-buggy caller already passes)
#      is unaffected: a clean, unheld worktree is still removed.
#
# NO DATABASE. spira_bead_status is driven through SPIRA_STATUS_SEAM (lib.sh), not a real
# bd — this suite proves the path-identity fence, not bead-status plumbing.
#
# defect: sp-87csm
# tier: T1
# covers: spira/lib.sh sending/src/*
# scar: a bead's own worktree, checked out on a branch inherited from its parent, was
#   destroyed the moment the parent's branch landed and the parent's own aeon had already
#   gone, because the liveness check asked only about the branch's bead, never the path's.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"; [ -n "${AEON_PID:-}" ] && kill "$AEON_PID" 2>/dev/null; true' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

export SPIRA_RUN="$T/run"; mkdir -p "$SPIRA_RUN/worktree"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_REAPLOG="$T/reap.log"
tl_config SPIRA_RUN="$SPIRA_RUN"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

REMOTE="$T/remote.git"; REPO="$T/repo"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
timeout 5 git -C "$REPO" push -q origin main

# Every id this suite ever asks spira_bead_status about, set through a real file rather
# than `spira_status_seam -`: a bare `-` reads from spira_status_seam's own stdin, which a
# caller invoked as a plain statement (no pipe) leaves attached to the suite's own stdin,
# not to any `printf` — so the read silently starves and SPIRA_STATUS_MAP stays empty. Both
# ids are known up front; no case needs to add one mid-run.
STATUS_FILE="$T/status.tsv"
printf 'sp-parent\tclosed\nsp-both\tclosed\n' > "$STATUS_FILE"
spira_status_seam "$STATUS_FILE"
# sp-mve9i: the claim witness reads the lifecycle row (`spira-lc show`), never bd status, and
# a machine that does not answer is "somebody may be home". This suite has no lifecycle store,
# so a stand-in answers its three beads: the closed ones handed on (SUBMITTED), the child
# READY — nobody holds any of them by the machine's word; only the planted aeon does.
mkdir -p "$T/lc"
cat > "$T/lc/spira-lc" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = show ] || exit 2
case "${2:-}" in
    sp-parent|sp-both) st=SUBMITTED ;;
    sp-child) st=READY ;;
    *) exit 1 ;;
esac
printf '{"bead":{"bead_id":"%s","state":"%s","holds":[]},"delivery":null}\n' "$2" "$st"
STUB
chmod +x "$T/lc/spira-lc"
export SPIRA_LC_BIN="$T/lc/spira-lc"

echo "test-worktree-branch-mismatch.sh"
echo

# ======================================================================================
echo "CASE 1 — path's own bead is live: REFUSED even though the caller's id is not held"
# ======================================================================================
git -C "$REPO" branch spira/sp-parent main
WT_CHILD="$SPIRA_RUN/worktree/sp-child"
git -C "$REPO" worktree add -q "$WT_CHILD" spira/sp-parent

# The parent (the caller's id, taken from the landed branch) has no aeon and reads closed
# (status seam, above) — the witness that used to be the whole answer.
#
# A live "aeon" holding the CHILD — the worktree's own directory name — via a pidfile whose
# cmdline genuinely contains aeon.sh, so aeon_alive's argv check is exercised for real
# rather than assumed (law-a-pattern-match-is-not-an-identity-check).
bash -c 'exec -a aeon.sh sleep 60' &
AEON_PID=$!
echo "$AEON_PID" > "$SPIRA_RUN/aeon-testfayth-sp-child.pid"
echo "$(( $(date +%s) + 3600 ))" > "$SPIRA_RUN/aeon-testfayth-sp-child.lease"
# Positive control: the pidfile really is alive before we rely on it.
is "planted aeon pid is alive" "0" "$([ -d "/proc/$AEON_PID" ]; echo $?)"

rc1=0
spira_destroy_worktree "sp-parent" "$WT_CHILD" "$REPO" "landed in main" 2>/dev/null || rc1=$?
is "spira_destroy_worktree refuses (path's own bead is live)" "1" "$rc1"
[ -d "$WT_CHILD" ] \
    && ok  "child worktree directory survives the refusal" \
    || bad "child worktree directory survives the refusal" "directory was removed"

log_entry="$(grep 'sp-parent' "$SPIRA_REAPLOG" 2>/dev/null | grep REFUSED | grep 'as sp-child' || true)"
[ -n "$log_entry" ] \
    && ok  "the refusal names the worktree's own bead, not just the caller's" \
    || bad "the refusal names the worktree's own bead, not just the caller's" "not found in $SPIRA_REAPLOG"

echo

# ======================================================================================
echo "CASE 2 — once the path's own aeon is gone, the same call proceeds"
# ======================================================================================
kill "$AEON_PID" 2>/dev/null
wait "$AEON_PID" 2>/dev/null || true
# holder_alive requires /proc/$pid to be gone, not just the process killed and unreaped.
for _ in 1 2 3 4 5 6 7 8 9 10; do [ -d "/proc/$AEON_PID" ] || break; sleep 0.2; done

rc2=0
spira_destroy_worktree "sp-parent" "$WT_CHILD" "$REPO" "landed in main" 2>/dev/null || rc2=$?
is "spira_destroy_worktree proceeds once nobody actually holds the path's bead" "0" "$rc2"
[ -d "$WT_CHILD" ] \
    && bad "child worktree directory is removed" "directory still exists" \
    || ok  "child worktree directory is removed"

echo

# ======================================================================================
echo "CASE 3 — ordinary case (id == the worktree's own name) is unaffected"
# ======================================================================================
git -C "$REPO" branch spira/sp-both main
WT_BOTH="$SPIRA_RUN/worktree/sp-both"
git -C "$REPO" worktree add -q "$WT_BOTH" spira/sp-both

rc3=0
spira_destroy_worktree "sp-both" "$WT_BOTH" "$REPO" "landed in main" 2>/dev/null || rc3=$?
is "ordinary matching-id removal still succeeds" "0" "$rc3"
[ -d "$WT_BOTH" ] \
    && bad "ordinary worktree is removed" "directory still exists" \
    || ok  "ordinary worktree is removed"

tl_summary
