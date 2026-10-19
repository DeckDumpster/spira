#!/usr/bin/env bash
#
# test-wiki-commit-only-named.sh — wiki-commit.sh commits the paths it was given and
# nothing else that happens to be staged in the index.
#
# Requires only git and bash — no database fixture.
#
# tier: T1
# covers: spira/wiki-commit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
WIKI_COMMIT="${WIKI_COMMIT_UNDER_TEST:-$HERE/wiki-commit.sh}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

WIKI="$TMP/wiki"; git init -q -b main "$WIKI"
printf 'seed\n' > "$WIKI/seed.md"; git -C "$WIKI" add seed.md; git -C "$WIKI" commit -qm seed

echo "test-wiki-commit-only-named.sh"
mkdir -p "$WIKI/wiki/notes"
printf 'other\n' > "$WIKI/wiki/log.md"
git -C "$WIKI" add wiki/log.md
printf 'law\n' > "$WIKI/wiki/notes/common-law.md"

rc=0
printf 'wiki/notes/common-law.md\n' | bash "$WIKI_COMMIT" "$WIKI" "law: enact" || rc=$?
is "commit succeeds" "0" "$rc"
is "HEAD names only the given path" "wiki/notes/common-law.md" \
    "$(git -C "$WIKI" show --name-only --format= HEAD)"
is "unrelated staged file stays staged" "A	wiki/log.md" \
    "$(git -C "$WIKI" diff --cached --name-status)"
is "unrelated file content untouched" "other" "$(cat "$WIKI/wiki/log.md")"

echo
tl_summary
