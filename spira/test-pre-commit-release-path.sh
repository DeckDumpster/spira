#!/usr/bin/env bash
#
# test-pre-commit-release-path.sh — the pre-commit hook finds the tools of the release it
# sits in, even under lifecycle_enforce's restricted PATH (/usr/bin:/bin:<release>/bin),
# which omits <release>/spira (sp-tf7nt).
#
# covers: spira/hooks/pre-commit spira/worktree-hooks.sh aeon/src/restrict.rs
# tier: T1
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# A fake release: the real hook in spira/hooks, stub tools beside it that record each call.
REL="$TMP/release"
mkdir -p "$REL/bin" "$REL/spira/hooks"
cp "$HERE/hooks/pre-commit" "$REL/spira/hooks/pre-commit"
chmod +x "$REL/spira/hooks/pre-commit"
LOG="$TMP/calls"
stub() { # stub <dir> <name> <rc for "staged">
    printf '#!/usr/bin/env bash\necho "%s $*" >> "%s"\n[ "${1:-}" = harness ] && exit 1\nexit %s\n' "$2" "$LOG" "$3" > "$1/$2"
    chmod +x "$1/$2"
}
stub "$REL/spira" exclude.sh 0
stub "$REL/spira" branch-guard.sh 0
stub "$REL/bin" spira-lint 0

REPO="$TMP/repo"
git init -q -b work "$REPO"
git -C "$REPO" config core.hooksPath "$REL/spira/hooks"
git -C "$REPO" config user.name t
git -C "$REPO" config user.email t@example.com
GIT_BIN="$(dirname "$(command -v git)")"

commit() { # commit <file> -> rc, under the restricted PATH shape
    printf 'x\n' > "$REPO/$1"
    git -C "$REPO" add "$1"
    env -i HOME="$TMP" PATH="/usr/bin:/bin:$GIT_BIN:$REL/bin" git -C "$REPO" commit -q -m "$1" >"$TMP/out" 2>&1
}

echo "test-pre-commit-release-path.sh — the hook runs its own release's tools under a restricted PATH"

: > "$LOG"
commit a.txt; rc=$?
is "a commit under the restricted PATH passes the hook" 0 "$rc"
want "exclude.sh beside the hook ran"      "exclude.sh staged"     "$(cat "$LOG")"
want "spira-lint from the release's bin ran" "spira-lint --only scratch-fence" "$(cat "$LOG")"
want "branch-guard.sh beside the hook ran" "branch-guard.sh staged" "$(cat "$LOG")"

# Positive control: the hook really consults those tools — one that refuses stops the commit.
stub "$REL/spira" exclude.sh 1
: > "$LOG"
commit b.txt; rc=$?
is "a refusing tool beside the hook refuses the commit" 1 "$rc"

# And a missing tool still refuses, naming it (never a silent skip).
rm "$REL/spira/branch-guard.sh"
stub "$REL/spira" exclude.sh 0
commit c.txt; rc=$?
is   "a missing tool refuses the commit" 1 "$rc"
want "and names it" "branch-guard.sh is not on PATH" "$(cat "$TMP/out")"

# THE COMPOSED WORKTREE HOOK (sp-djgb4). An aeon worktree's pre-commit is written by
# worktree-hooks.sh: the canonical hook, then pre-commit-guard.sh — which must be found the
# same way, beside the hooks, not by bare name on the restricted PATH.
stub "$REL/spira" exclude.sh 0
stub "$REL/spira" branch-guard.sh 0
stub "$REL/spira" pre-commit-guard.sh 0
cp "$HERE/worktree-hooks.sh" "$REL/spira/worktree-hooks.sh"
WREPO="$TMP/wrepo"
git init -q -b main "$WREPO"
git -C "$WREPO" config user.name t
git -C "$WREPO" config user.email t@example.com
git -C "$WREPO" commit -q --allow-empty -m init
git -C "$WREPO" worktree add -q -b work "$TMP/wt"
git -C "$TMP/wt" config user.name t
git -C "$TMP/wt" config user.email t@example.com
SPIRA_HOME="$REL/spira" bash "$REL/spira/worktree-hooks.sh" install "$TMP/wt" >/dev/null 2>&1
is "worktree-hooks.sh arms the worktree" 0 "$?"
: > "$LOG"
printf 'x\n' > "$TMP/wt/d.txt"; git -C "$TMP/wt" add d.txt
env -i HOME="$TMP" PATH="/usr/bin:/bin:$GIT_BIN:$REL/bin" git -C "$TMP/wt" commit -q -m d >"$TMP/out" 2>&1; rc=$?
is "a worktree commit under the restricted PATH passes both hooks" 0 "$rc"
want "pre-commit-guard.sh beside the hooks ran" "pre-commit-guard.sh" "$(cat "$LOG")"

tl_summary
