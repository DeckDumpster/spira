#!/usr/bin/env bash
# wiki-hooks-install.sh — point the wiki checkout's core.hooksPath at spira/wiki-hooks.
#
# Arms wiki-lint-links.sh as a real pre-commit hook, so a commit that adds an index.md
# wikilink to a page the commit doesn't track fails instead of succeeding — whoever makes
# that commit, including an archivist session whose own commit is prose-issued shell, not
# code this harness runs.
#
# Points at $SPIRA_HOME rather than the caller's worktree: the wiki checkout is long-lived
# and the hooks path must still resolve after this worktree is torn down.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

wiki="${1:-$SPIRA_WIKI}"
[ -n "$wiki" ] || { echo "wiki-hooks-install.sh: no wiki dir (pass one or set SPIRA_WIKI)" >&2; exit 1; }
git -C "$wiki" rev-parse --git-dir >/dev/null 2>&1 || {
    echo "wiki-hooks-install.sh: $wiki is not a git repository" >&2; exit 1; }

hooks="$SPIRA_HOME/spira/wiki-hooks"
[ -x "$hooks/pre-commit" ] || {
    echo "wiki-hooks-install.sh: no executable hook at $hooks/pre-commit — hook NOT armed" >&2
    exit 1
}
git -C "$wiki" config core.hooksPath "$hooks"
echo "wiki-hooks-install.sh: $wiki core.hooksPath -> $hooks"
