#!/usr/bin/env bash
#
# test-aeon-wiki-concurrent.sh — two writers sharing one wiki checkout must each get their
# own commit; neither can commit the other's staged paths.
#
# THE DEFECT. Two concurrent aeon exits both call the wiki commit section. Writer A stages
# its files; before A commits, Writer B stages its own files and runs `git commit`. Because
# the git index is shared, B's commit includes A's staged files. A's subsequent commit finds
# nothing staged. The result: A's work lands under B's author and message, or is silently
# lost.
#
# POSITIVE CONTROL. The interleave (add, race-commit, add, commit) is reproduced at the git
# level to confirm the underlying race is real. This is the offender; its presence earns the
# silence in the concurrent test below.
#
# FIXED BEHAVIOR. wiki-commit.sh acquires an exclusive flock on the checkout's
# .git/spira-commit.lock before staging, so concurrent callers are serialised: each writer
# commits only the paths it provided, under its own message.
#
# AN ACTUAL INTERLEAVE IS FORCED (gap G16's wiki half), not hoped for. Launching both
# writers in the background and waiting can pass even with the lock removed, if the OS
# scheduler simply happens to run one to completion before the other starts — no real
# contention, no real test. Here the test itself pre-acquires spira-commit.lock BEFORE
# either writer starts, so both are guaranteed to reach flock() and block on the SAME lock
# this process holds; releasing it is what lets exactly one proceed at a time. Against an
# unlocked wiki-commit.sh this hold has no effect at all — both writers race the git index
# immediately, exactly as the positive control demonstrates.
#
# Requires only git and bash — no database fixture.
#
# covers: spira/wiki-commit.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
WIKI_COMMIT="$HERE/wiki-commit.sh"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

[ -f "$WIKI_COMMIT" ] || { printf 'FATAL: wiki-commit.sh not found at %s\n' "$WIKI_COMMIT" >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# ---- shared wiki repo -----------------------------------------------------------------
WIKI_ORIGIN="$TMP/wiki.git"; git init -q --bare -b main "$WIKI_ORIGIN"
WIKI="$TMP/wiki"; git clone -q "$WIKI_ORIGIN" "$WIKI" 2>/dev/null
git -C "$WIKI" config user.email t@t; git -C "$WIKI" config user.name t
mkdir -p "$WIKI/wiki"
printf 'seed\n' > "$WIKI/wiki/seed.md"
git -C "$WIKI" add wiki/seed.md
git -C "$WIKI" commit -qm "seed"
git -C "$WIKI" push -q origin main 2>/dev/null
SEED_SHA="$(git -C "$WIKI" rev-parse HEAD)"

echo "test-aeon-wiki-concurrent.sh"

# =======================================================================================
echo
echo "POSITIVE CONTROL: raw git race — add is not atomic with commit on a shared index:"
echo "-----------------------------------------------------------------------"
# SEEN RED FIRST. This section is the offender that proves the race is real.
# Without wiki-commit.sh's lock, the interleave below produces wrong attribution.
#   writer-a stages file-a, writer-b stages file-b and commits (picking up file-a),
#   writer-a's commit finds nothing staged.
printf 'content-a\n' > "$WIKI/wiki/race-a.md"
git -C "$WIKI" add -- wiki/race-a.md 2>/dev/null       # writer-a stages its file

printf 'content-b\n' > "$WIKI/wiki/race-b.md"
git -C "$WIKI" add -- wiki/race-b.md 2>/dev/null       # writer-b stages its file
git -C "$WIKI" \
    -c user.email=writer-b@spira.local -c user.name=writer-b \
    commit -qm "writer-b: writes" 2>/dev/null          # writer-b commits both

# writer-a's commit now finds nothing in the index.
a_out="$(git -C "$WIKI" commit -m "writer-a: writes" 2>&1 || true)"
a_last_msg="$(git -C "$WIKI" log --format="%s" -1 2>/dev/null)"
a_last_files="$(git -C "$WIKI" show --name-only --format="" HEAD 2>/dev/null)"

want "positive-control: race-a.md ended up in writer-b's commit" "race-a.md" "$a_last_files"
want "positive-control: that commit bears writer-b's message"    "writer-b"  "$a_last_msg"
want "positive-control: writer-a's commit found nothing staged"  "nothing"   "$a_out"

# Reset to seed state for subsequent cases.
git -C "$WIKI" reset --hard "$SEED_SHA" 2>/dev/null
git -C "$WIKI" clean -qfd 2>/dev/null

# =======================================================================================
echo
echo "CASE 1: concurrent wiki-commit.sh calls — each writer must get its own commit:"
echo "-----------------------------------------------------------------------"
# SEEN RED FIRST: without the flock in wiki-commit.sh this test fails because one process
# commits the other's staged file or produces a commit with the wrong message.

# THE FORCED INTERLEAVE. Hold the exact lock wiki-commit.sh acquires BEFORE either writer
# starts, so both are guaranteed to still be blocked in flock() when we release it below —
# a real race on the real lock, not a hopeful backgrounding of two processes.
exec {_hold_fd}>"$WIKI/.git/spira-commit.lock"
flock "$_hold_fd"

# writer-a: write its file and attempt to commit — blocks on our held lock.
(
    export GIT_AUTHOR_NAME=writer-a GIT_AUTHOR_EMAIL=writer-a@spira.local
    export GIT_COMMITTER_NAME=writer-a GIT_COMMITTER_EMAIL=writer-a@spira.local
    printf 'content-a\n' > "$WIKI/wiki/concurrent-a.md"
    printf 'wiki/concurrent-a.md\n' | bash "$WIKI_COMMIT" "$WIKI" "writer-a: writes"
) &
pid_a=$!

# writer-b: write its file and attempt to commit — blocks on the same held lock.
(
    export GIT_AUTHOR_NAME=writer-b GIT_AUTHOR_EMAIL=writer-b@spira.local
    export GIT_COMMITTER_NAME=writer-b GIT_COMMITTER_EMAIL=writer-b@spira.local
    printf 'content-b\n' > "$WIKI/wiki/concurrent-b.md"
    printf 'wiki/concurrent-b.md\n' | bash "$WIKI_COMMIT" "$WIKI" "writer-b: writes"
) &
pid_b=$!

# Give both background jobs time to reach flock() and start waiting on our hold (best
# effort; the lock guarantees correctness regardless of exactly how long this is).
sleep 0.3
# Release: exactly one writer proceeds at a time from here.
exec {_hold_fd}>&-

wait "$pid_a" && rc_a=0 || rc_a=$?
wait "$pid_b" && rc_b=0 || rc_b=$?

is "writer-a exited 0" "0" "$rc_a"
is "writer-b exited 0" "0" "$rc_b"

a_msg="$(git -C "$WIKI" log --format="%s" -- wiki/concurrent-a.md 2>/dev/null | head -1)"
b_msg="$(git -C "$WIKI" log --format="%s" -- wiki/concurrent-b.md 2>/dev/null | head -1)"
is "concurrent-a.md bears writer-a's message" "writer-a: writes" "$a_msg"
is "concurrent-b.md bears writer-b's message" "writer-b: writes" "$b_msg"

# Each file must be in a separate commit (two commits since seed).
commit_count="$(git -C "$WIKI" log --oneline "$SEED_SHA..HEAD" 2>/dev/null | wc -l | tr -d ' ')"
is "two commits produced (one per writer)" "2" "$commit_count"

# =======================================================================================
echo
echo "CASE 2: no-op when file already committed before lock is acquired:"
echo "-----------------------------------------------------------------------"
# If a caller's _wc_new contains a file already committed by a concurrent writer (because
# git-status was sampled before the other writer committed), wiki-commit.sh must not create
# a duplicate commit for that file.
printf 'content-c\n' > "$WIKI/wiki/already-committed.md"
git -C "$WIKI" add -- wiki/already-committed.md 2>/dev/null
git -C "$WIKI" commit -qm "someone-else: committed already"

commits_before="$(git -C "$WIKI" rev-parse HEAD)"
# writer-c has already-committed.md in its file list (stale _wc_new), but the file
# is now committed — wiki-commit.sh should stage it (git add is a no-op) and find
# nothing new to commit.
rc_c=0
printf 'wiki/already-committed.md\n' | bash "$WIKI_COMMIT" "$WIKI" "writer-c: writes" || rc_c=$?
is "no-op commit exits 0 (already committed file)" "0" "$rc_c"
is "no new commit created for already-committed file" \
    "$commits_before" "$(git -C "$WIKI" rev-parse HEAD)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
