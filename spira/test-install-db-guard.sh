#!/usr/bin/env bash
#
# test-install-db-guard.sh — install.sh phase 3 refuses a database inside a git checkout,
#   but NOT the database's own repository that `bd init` creates.
#
# THE DEFECT (acceptance phases B and D). `bd init` (bd 1.2.1) runs `git init` in the
# directory it initialises and stages its own files there — so after the first install,
# $SPIRA_DB IS a git repository, and install.sh's own phase 3 check ("REFUSING database at
# $SPIRA_DB — it is inside a git checkout ($SPIRA_DB)") refused every later install of the
# same instance: acceptance phase B's predecessor install exited 2, and so would any re-run
# of install.sh on a production box (its database carries the same bd-made repository).
#
# WHAT THE REFUSAL IS FOR, and what therefore still refuses: a database inside SOMEONE'S
# checkout is one `git add -A` from publishing every bead body (law-beads-is-never-public).
#   - a .git in any directory ABOVE the database            -> refuse
#   - the database's own .git that has a remote configured  -> refuse (one push from public)
#   - the database's own .git with no remote (bd init's)    -> allowed
#   - no .git anywhere                                      -> allowed
#
# tier: T1
# covers: install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-install-db-guard.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# shellcheck disable=SC1091
. "$HERE/../install.sh"
declare -f _db_git_guard >/dev/null \
    || { printf 'test-install-db-guard: install.sh defines no _db_git_guard\n' >&2; bad "install.sh defines _db_git_guard" "absent"; tl_summary; exit 1; }

guard() { _db_git_guard "$1" 2>"$TMP/err"; }

# ---- POSITIVE CONTROLS: the refusal still fires where it must ---------------------------
mkdir -p "$TMP/checkout/data/db"; git init -q "$TMP/checkout"
guard "$TMP/checkout/data/db"
wantrc "a database inside a parent git checkout is refused" 2 $?
want   "and the refusal names the checkout" "$TMP/checkout" "$(cat "$TMP/err")"

mkdir -p "$TMP/remoted/db"; git init -q "$TMP/remoted/db"
git -C "$TMP/remoted/db" remote add origin "https://example.invalid/beads.git"
guard "$TMP/remoted/db"
wantrc "the database's own repository WITH a remote is refused" 2 $?

# ---- THE DEFECT: the database's own repository, as bd init leaves it --------------------
mkdir -p "$TMP/own/db"; git init -q "$TMP/own/db"
printf 'x\n' > "$TMP/own/db/AGENTS.md"; git -C "$TMP/own/db" add -A
git -C "$TMP/own/db" commit -qm "bd init: initialize beads issue tracking"
guard "$TMP/own/db"
wantrc "the database's own repository with no remote is allowed" 0 $?

# THE REAL THING, when bd is here: whatever this bd's init leaves behind must pass the
# guard, or the second install of every instance refuses.
if command -v bd >/dev/null 2>&1; then
    mkdir -p "$TMP/real/db"
    ( cd "$TMP/real/db" && HOME="$TMP/real" BEADS_NO_AUTO_IMPORT=1 BD_NON_INTERACTIVE=1 \
        timeout 120 bd init --prefix tt </dev/null >/dev/null 2>&1 )
    if [ -d "$TMP/real/db/.git" ]; then
        ok "bd init makes the database directory a git repository (the condition under test)"
    else
        ok "bd init left no repository here (older/newer bd) — the guard is trivially clear"
    fi
    guard "$TMP/real/db"
    wantrc "a database fresh from bd init passes the guard (a second install is not refused)" 0 $?
fi

# ---- no git at all -----------------------------------------------------------------------
mkdir -p "$TMP/plain/db"
guard "$TMP/plain/db"
wantrc "a database with no repository anywhere is allowed" 0 $?
guard ""
wantrc "an unset SPIRA_DB is not judged" 0 $?

tl_summary
