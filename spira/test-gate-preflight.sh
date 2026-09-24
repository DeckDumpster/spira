#!/usr/bin/env bash
#
# test-gate-preflight.sh — the gate's preflight checks fire with their evidence.
#
#   ./test-gate-preflight.sh
#
# THE DEFECT THIS PREVENTS. The base..branch diff suppressed git's stderr and then exited 1
# bare (sp-io5j), so an unresolvable branch produced a 13-byte log holding only the exit
# code. The gate now captures git's own diagnostic and includes it, so a missing branch or
# an unfetched base names itself rather than saying nothing.
#
# defect: sp-io5j
# tier: T1
# covers: spira/gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
MAP="$TMP/repo-map"; VDIR="$TMP/verdicts"; GATELOG="$TMP/gate.log"; HOMEDIR="$TMP/home"
mkdir -p "$RUN/worktree" "$HOMEDIR" "$SH"

cp "$HERE/gate.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/exclude.sh" "$HERE/skew.sh" \
   "$HERE/yield.sh" "$SH/"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
printf 'base\n' > "$REPO/marker"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin

# A real branch so the positive control can pass.
BR=spira/sp-pre1
W="$TMP/work"
git -C "$REPO" worktree add -q -b "$BR" "$W" origin/main
printf 'work\n' > "$W/f1.txt"
git -C "$W" add -A; git -C "$W" commit -q -m "feat: sp-pre1 — work"
git -C "$REPO" worktree remove --force "$W"

printf 'repo | %s | push | origin/main |  | true\n' "$REPO" > "$MAP"

rungate() {              # rungate <branch> [VAR=VAL ...]
    local br="$1"; shift
    env -i HOME="$HOMEDIR" PATH="/usr/bin:/bin" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_REPO_MAP="$MAP" SPIRA_GATE_LOG="$GATELOG" \
        SPIRA_VERDICTS="$VDIR" SPIRA_VERDICT_TTL=0 \
        "$@" bash "$SH/gate.sh" "$br" repo 2>&1
}

echo "test-gate-preflight.sh — the gate's preflight checks fire with their evidence"

# --------------------------------------------------------------------------------------
# POSITIVE CONTROL. Before claiming the gate catches a bad diff, show it passes a good one.
# A suite that only checked "the gate refused" would pass against a gate that refused
# everything (law-absence-needs-a-positive-control).
# --------------------------------------------------------------------------------------
out="$(rungate "$BR")"; rc=$?
is   "a valid branch passes the gate"  0 "$rc"
want "and says PASS"                   "VERDICT=PASS" "$out"

# --------------------------------------------------------------------------------------
# CASE 1 — AN UNRESOLVABLE BRANCH. The base is valid (origin/main) but the branch does not
# exist. git diff says exactly what is wrong — "fatal: ambiguous argument ... unknown
# revision" — and the gate must include that rather than discarding it.
# --------------------------------------------------------------------------------------
out="$(rungate "spira/no-such-branch")"; rc=$?
is   "an unresolvable branch exits NO_VERDICT"       75 "$rc"
want "and names the reason as no-diff"                "reason=no-diff" "$out"
want "and the verdict line says NO_VERDICT"           "VERDICT=NO_VERDICT" "$out"
want "and the message names the base and branch"      "origin/main...spira/no-such-branch" "$out"
want "and the message names the repo"                 "repo" "$out"
want "and git's own diagnostic is included"           "fatal:" "$out"

# --------------------------------------------------------------------------------------
# CASE 2 — AN UNRESOLVABLE BASE. The branch exists but the base ref in the repo-map points
# at a ref that was never fetched. The diff fails for the same reason — one side does not
# resolve — and the message must say which.
# --------------------------------------------------------------------------------------
printf 'repo | %s | push | refs/remotes/origin/no-such-base |  | true\n' "$REPO" > "$MAP"
out="$(rungate "$BR")"; rc=$?
is   "an unresolvable base exits NO_VERDICT"          75 "$rc"
want "with reason no-diff or no-base"                 "NO_VERDICT" "$out"

# Restore the map for any future cases.
printf 'repo | %s | push | origin/main |  | true\n' "$REPO" > "$MAP"
tl_summary
