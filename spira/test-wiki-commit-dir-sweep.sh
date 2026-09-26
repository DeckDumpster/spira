#!/usr/bin/env bash
#
# test-wiki-commit-dir-sweep.sh — a collapsed directory path must never reach a bare
# `git add`, because that stages every file beneath it, not the file the caller named.
#
#   ./test-wiki-commit-dir-sweep.sh
#
# THE DEFECT (sp-0wow1). `git status --short` collapses a wholly-untracked directory to
# one line naming the directory, not its files. aeon.sh's wiki dirty-diff read that output
# and handed the result straight to wiki-commit.sh as a list of "paths it wrote". When the
# directory line named someone else's uncommitted work — a maechen pass swept 48 files
# from the concierge's still-under-review design pages this way (brain c7c79af) — the
# commit picked up every file under it, none of which the committing session wrote.
# `git add -- <dir>/` recurses exactly like `git add -A` for that subtree; it is not one
# of the literal forms wiki-add-fence.sh scans for, so that fence never saw it.
#
# THE FIX. wiki-commit.sh refuses any staged path that is a directory rather than adding
# it, and aeon.sh now takes its dirty-diff with `--untracked-files=all` so a wholly-new
# directory is named file by file and this refusal is never hit in the ordinary case.
#
# POSITIVE CONTROL FIRST. Case 1 plants exactly the collapsed-directory input the old
# `git status --short` would have produced and requires wiki-commit.sh to refuse it and
# leave the directory's files uncommitted (law-absence-needs-a-positive-control). Case 2
# proves the refusal does not cost a legitimate, explicitly-named sibling file in the same
# call.
#
# Requires only git and bash — no database fixture.
#
# tier: T1
# covers: spira/wiki-commit.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
WIKI_COMMIT="$HERE/wiki-commit.sh"

[ -f "$WIKI_COMMIT" ] || { printf 'FATAL: wiki-commit.sh not found at %s\n' "$WIKI_COMMIT" >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

WIKI_ORIGIN="$TMP/wiki.git"; git init -q --bare -b main "$WIKI_ORIGIN"
WIKI="$TMP/wiki"; git clone -q "$WIKI_ORIGIN" "$WIKI" 2>/dev/null
git -C "$WIKI" config user.email t@t; git -C "$WIKI" config user.name t
printf 'seed\n' > "$WIKI/wiki-seed.md"
git -C "$WIKI" add wiki-seed.md
git -C "$WIKI" commit -qm seed
git -C "$WIKI" push -q origin main 2>/dev/null

echo "test-wiki-commit-dir-sweep.sh"

# =======================================================================================
echo
echo "POSITIVE CONTROL: a collapsed directory line must be refused, not swept:"
echo "-----------------------------------------------------------------------"
# Someone else's uncommitted work: a whole untracked directory with several files —
# exactly what \`git status --short\` (no -uall) collapses to one line for.
mkdir -p "$WIKI/wiki/notes/designs/someone-elses-draft"
printf 'draft a\n' > "$WIKI/wiki/notes/designs/someone-elses-draft/a.md"
printf 'draft b\n' > "$WIKI/wiki/notes/designs/someone-elses-draft/b.md"
printf 'draft c\n' > "$WIKI/wiki/notes/designs/someone-elses-draft/c.md"

# SEEN RED FIRST: feed wiki-commit.sh the collapsed directory path, as the old
# `status --short` (no -uall) would have produced it.
_stderr="$(printf 'wiki/notes/designs/someone-elses-draft/\n' \
    | bash "$WIKI_COMMIT" "$WIKI" "sweep attempt" 2>&1 1>/dev/null)"

want "positive control: refusal is logged" "refusing directory path" "$_stderr"
is "positive control: no file from the draft is committed" "" \
    "$(git -C "$WIKI" log --all --format='' --name-only -- wiki/notes/designs/someone-elses-draft 2>/dev/null)"
is "positive control: draft files remain untracked" \
    "?? wiki/notes/designs/someone-elses-draft/" \
    "$(git -C "$WIKI" status --short --untracked-files=normal -- wiki/notes/designs/someone-elses-draft 2>/dev/null)"

# =======================================================================================
echo
echo "CASE: an explicitly-named file in the same call still commits:"
echo "-----------------------------------------------------------------------"
printf 'my own page\n' > "$WIKI/wiki/mine.md"
rc2=0
printf 'wiki/notes/designs/someone-elses-draft/\nwiki/mine.md\n' \
    | bash "$WIKI_COMMIT" "$WIKI" "legit write" || rc2=$?
is "named file commits despite the refused directory in the same call" "0" "$rc2"
is "wiki/mine.md is committed" "" \
    "$(git -C "$WIKI" diff --name-only HEAD -- wiki/mine.md 2>/dev/null)"
_msg="$(git -C "$WIKI" log --format='%s' -1 -- wiki/mine.md 2>/dev/null)"
is "commit message is the caller's" "legit write" "$_msg"
is "the draft directory is still not in that commit" "" \
    "$(git -C "$WIKI" show --name-only --format='' HEAD -- wiki/notes/designs/someone-elses-draft 2>/dev/null)"

echo
tl_summary
