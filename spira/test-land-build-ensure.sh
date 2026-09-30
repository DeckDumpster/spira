#!/usr/bin/env bash
#
# test-land-build-ensure.sh — land-build-ensure.sh fires build.sh when cargo source changed.
#
# The "binary missing while its unit is enabled" trigger (sp-2mn5v) is deleted with it
# (sp-gypjk): a release always carries every binary, so there is no missing-binary state
# left to heal, and the suite's cases for it went with the trigger.
#
# gap G10: the cargo-source-changed trigger, read from the reflog one step behind the
# checkout's current branch (`git rev-parse "$branch@{1}"`), over a real git repo: skew.sh's
# own refresh() advances the checkout with `git reset --mixed`, which is exactly what writes
# that reflog entry, so this drives the same primitive rather than a hand-written model.
#
# build.sh is found by name on PATH (sp-gypjk), so the stub build.sh sits first on PATH.
#
# covers: spira/land-build-ensure.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

STUBS="$TMP/stubs"; mkdir -p "$STUBS"
BUILD_LOG="$TMP/build.log"
printf '#!/usr/bin/env bash\nprintf "built\\n" >> "%s"\n' "$BUILD_LOG" > "$STUBS/build.sh"
chmod +x "$STUBS/build.sh"

built() { [ -f "$BUILD_LOG" ]; }

# ============================================================================
# gap G10: the reflog-based source-changed trigger, over a real git repo.
# ============================================================================
GITREPO="$TMP/gitrepo"
mkdir -p "$GITREPO/spira"
git -C "$GITREPO" init -q -b maintrig
git -C "$GITREPO" config user.email t@t
git -C "$GITREPO" config user.name t
printf 'seed\n' > "$GITREPO/seed.txt"
git -C "$GITREPO" add -A && git -C "$GITREPO" commit -q -m seed

run_ensure_git() {
    rm -f "$BUILD_LOG"
    ( cd "$TMP" && env -i \
        PATH="$STUBS:$PATH" \
        SPIRA_REPO="$GITREPO" \
        land-build-ensure.sh >"$TMP/out.log" 2>&1 )
}

# POSITIVE CONTROL: the checkout moves (a real reflog entry is written) but the
# diff between the reflog's previous position and HEAD touches no .rs file —
# proving this is a name-filtered diff, not "any movement rebuilds".
printf 'plain\n' > "$GITREPO/plain.txt"
git -C "$GITREPO" add -A && git -C "$GITREPO" commit -q -m "non-rust change"
run_ensure_git
if built; then
    bad "a non-.rs commit must not trigger the reflog-based rebuild"
else
    ok "positive control: a non-.rs commit does not trigger the reflog-based rebuild (checkout still moved)"
fi

# THE TRIGGER: a commit that touches a .rs file fires build.sh, read from the
# branch's own reflog one step back — the same primitive skew.sh's refresh()
# advances the checkout with (git reset --mixed) rather than a hand-modeled diff.
mkdir -p "$GITREPO/loom/src"
printf 'fn main() {}\n' > "$GITREPO/loom/src/main.rs"
git -C "$GITREPO" add -A && git -C "$GITREPO" commit -q -m "rust change"
run_ensure_git
if built; then
    ok "a .rs-touching commit triggers build.sh via the branch's own reflog"
else
    bad "a .rs-touching commit must trigger build.sh via the branch's own reflog"
fi

# A detached HEAD (no branch to read a reflog "one step back" from) never
# reaches the diff at all — symbolic-ref fails closed rather than guessing.
git -C "$GITREPO" checkout -q --detach HEAD
printf 'fn other() {}\n' >> "$GITREPO/loom/src/main.rs"
git -C "$GITREPO" add -A && git -C "$GITREPO" commit -q -m "rust change on a detached HEAD"
run_ensure_git
if built; then
    bad "a detached HEAD must not trigger the reflog-based rebuild"
else
    ok "a detached HEAD never reaches the reflog-based rebuild (symbolic-ref fails closed)"
fi

tl_summary
