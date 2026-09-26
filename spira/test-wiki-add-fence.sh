#!/usr/bin/env bash
#
# test-wiki-add-fence.sh — positive control for the wiki blanket-add fence.
#
#   ./test-wiki-add-fence.sh
#
# THE DEFECT THIS GUARDS AGAINST. A blanket git-add on the wiki checkout
# (git add -A, git add ., git commit -a) sweeps another actor's uncommitted
# work into the commit, manufacturing false attribution (incident: sp-4fl2e).
# wiki-add-fence.sh detects any non-comment line that pairs SPIRA_WIKI with
# such a form.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION. A fence that reports a clean
# tree is indistinguishable from one whose matcher never fires. An offender is
# planted in a scratch repository, the fence is required to name it (SEEN RED),
# the plant is withdrawn, and only then is the fence's silence evidence of a
# clean tree (law-absence-needs-a-positive-control).
#
# host-reason: tests wiki-add-fence.sh against scratch git repositories only
#
# tier: T0
# covers: spira/wiki-add-fence.sh spira/gate-spira.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-wiki-add-fence.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

fence_at() {
    local root="$1"
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$root/spira/wiki-add-fence.sh" 2>&1
}

# ---------------------------------------------------------------------------------------
# EMPTY INDEX — must exit 3 (law-absence-needs-a-positive-control).
# ---------------------------------------------------------------------------------------
ROOT="$TMP/root"; mkdir -p "$ROOT/spira"
cp "$HERE/wiki-add-fence.sh" "$ROOT/spira/wiki-add-fence.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

out="$(fence_at "$ROOT")"; rc=$?
is   "empty index exits 3" "3" "$rc"
want "and says why"        "refusing to report clean" "$out"

# Seed with one clean file so subsequent checks have a non-empty index.
printf '#!/usr/bin/env bash\necho ok\n' > "$ROOT/spira/helper.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m init

out="$(fence_at "$ROOT")"; rc=$?
is   "clean tree exits 0" "0" "$rc"
want "and says so"        "no blanket-add" "$out"

# ---------------------------------------------------------------------------------------
# SEEN RED: git add -A on a code line that references SPIRA_WIKI.
# ---------------------------------------------------------------------------------------
cat > "$ROOT/spira/broken-wiki.sh" << 'BROKEN'
#!/usr/bin/env bash
git -C "$SPIRA_WIKI" add -A
git -C "$SPIRA_WIKI" commit -m "wiki writes"
BROKEN
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "plant add -A offender"

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN RED: add -A refused" "1" "$rc"
want "names the offending file" "broken-wiki.sh" "$out"
want "mentions add -A"          "add -A" "$out"

# ---------------------------------------------------------------------------------------
# SEEN GREEN: withdraw the offender.
# ---------------------------------------------------------------------------------------
git -C "$ROOT" rm -qf spira/broken-wiki.sh
git -C "$ROOT" commit -q -m "withdraw add -A offender"

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN GREEN: clean after withdrawal" "0" "$rc"
want "and says so"                        "no blanket-add" "$out"

# ---------------------------------------------------------------------------------------
# SEEN RED: git add . combined with SPIRA_WIKI.
# ---------------------------------------------------------------------------------------
cat > "$ROOT/spira/broken-wiki2.sh" << 'BROKEN'
#!/usr/bin/env bash
git -C "$SPIRA_WIKI" add .
BROKEN
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "plant add . offender"

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN RED: add . refused"  "1" "$rc"
want "names the file"           "broken-wiki2.sh" "$out"

git -C "$ROOT" rm -qf spira/broken-wiki2.sh
git -C "$ROOT" commit -q -m "withdraw add . offender"

# ---------------------------------------------------------------------------------------
# COMMENT LINES ARE NOT FLAGGED. Documentation of the defect must not trigger
# the fence — only executable code does.
# ---------------------------------------------------------------------------------------
cat > "$ROOT/spira/with-comment.sh" << 'SAFE'
#!/usr/bin/env bash
# do not use git -C "$SPIRA_WIKI" add -A — it sweeps unrelated work
printf '%s\n' "$SPIRA_WIKI"
SAFE
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "plant comment-only reference"

out="$(fence_at "$ROOT")"; rc=$?
is   "comment line not flagged" "0" "$rc"

git -C "$ROOT" rm -qf spira/with-comment.sh
git -C "$ROOT" commit -q -m "remove comment-only file"

# ---------------------------------------------------------------------------------------
# GATE INTEGRATION: wiki-add-fence.sh is referenced in gate-spira.sh.
# ---------------------------------------------------------------------------------------
if grep -q "wiki-add-fence" "$HERE/gate-spira.sh" 2>/dev/null; then
    ok "gate-spira.sh references wiki-add-fence.sh"
else
    bad "gate-spira.sh references wiki-add-fence.sh" "not found in gate-spira.sh"
fi
tl_summary
