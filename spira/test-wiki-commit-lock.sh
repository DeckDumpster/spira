#!/usr/bin/env bash
#
# test-wiki-commit-lock.sh — wiki-commit.sh never reports success for paths it did not commit:
# a failed `git add` (a held .git/index.lock) is a refusal that names the lock's age, and a
# path the index does not know is a refusal, not "nothing to commit".
#
# tier: T2
# covers: spira/wiki-commit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
W="$TMP/wiki"
git init -q "$W"
git -C "$W" config user.email t@example.com; git -C "$W" config user.name t
printf 'seed\n' > "$W/seed.md"; git -C "$W" add seed.md; git -C "$W" commit -qm seed
run() { WIKI_COMMIT_LOCK_WAIT=1 bash "$HERE/wiki-commit.sh" "$W" "msg" 2>&1; }

printf 'x\n' > "$W/page.md"
: > "$W/.git/index.lock"
out="$(printf 'page.md\n' | run)"; rc=$?
is   "stale index.lock: exit is non-zero" 1 "$([ "$rc" -ne 0 ] && echo 1 || echo 0)"
want "stale index.lock: names the lock" "index.lock" "$out"
want "stale index.lock: reports the lock age" "s old" "$out"
is   "stale index.lock: nothing committed" "seed" "$(git -C "$W" log -1 --format=%s)"
rm -f "$W/.git/index.lock"

out="$(printf 'page.md\n' | run)"; rc=$?
is   "no lock: exits 0" 0 "$rc"
is   "no lock: file committed" "msg" "$(git -C "$W" log -1 --format=%s)"

out="$(printf 'page.md\n' | run)"; rc=$?
is   "already committed: nothing to commit exits 0" 0 "$rc"

out="$(printf 'missing.md\n' | run)"; rc=$?
is   "unknown path: refused, not a no-op" 1 "$([ "$rc" -ne 0 ] && echo 1 || echo 0)"
printf 'ign.md\n' > "$W/.gitignore"; printf 'i\n' > "$W/ign.md"
out="$(printf 'ign.md\n' | WIKI_COMMIT_LOCK_WAIT=1 bash "$HERE/wiki-commit.sh" "$W" m 2>&1)"; rc=$?
is   "ignored path: refused, not a no-op" 1 "$([ "$rc" -ne 0 ] && echo 1 || echo 0)"

: > "$W/.git/index.lock"; printf 'r\n' > "$W/retry.md"
( sleep 1; rm -f "$W/.git/index.lock" ) &
out="$(printf 'retry.md\n' | WIKI_COMMIT_LOCK_WAIT=20 bash "$HERE/wiki-commit.sh" "$W" retried 2>&1)"; rc=$?
wait
is   "lock cleared during wait: exits 0" 0 "$rc"
is   "lock cleared during wait: committed" "retried" "$(git -C "$W" log -1 --format=%s)"
tl_summary
