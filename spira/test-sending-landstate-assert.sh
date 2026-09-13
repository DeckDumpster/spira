#!/usr/bin/env bash
#
# test-sending-landstate-assert.sh — the Sending logs an assertion when it reaps a branch
# with no landstate record, and stays silent when a record exists.
#
#   ./test-sending-landstate-assert.sh
#
# WHY THIS EXISTS. landing.sh writes $SPIRA_RUN/landstate/$id on every code path that
# processes a branch: gate, rebase conflict, or actual push. A branch that reaches the
# Sending with content already on the base but no landstate record means landing.sh never
# selected it for its loop — a selection bug. That is the shape of sp-qj8n: four
# close/reopen cycles, no landstate entry on any of them. The assertion added to sending.sh
# must fire on the FIRST reap so the anomaly is visible before any reopen cycle.
#
# THE PROPERTY UNDER TEST IS A DISTINCTION, NOT A VALUE. Two branches are set up: one with
# no landstate record and one with one. The suite asserts the log line fires for the first
# and not for the second. Without the second case the assertion could fire unconditionally
# and the suite would still pass.
#
# NO DATABASE, and the fixture git state is minimal: a base branch with a commit already on
# it, and a spira/* branch that ancestry says is already contained. content_landed can
# answer YES to a branch that is an ancestor of the base — the simplest shape — so no
# merge-tree machinery needs to be tested here. The assertion is in sending.sh; this suite
# is about when it fires, not about how content_landed reaches its conclusion.
#
# defect: sp-qj8n
# covers: spira/sending.sh
# scar: sending.sh reaped content-landed branches without a landstate record silently; the selection gap behind sp-qj8n's four close/reopen cycles was invisible until the assertion was added.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sending-landstate-assert
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sendlandstate || { echo "test-sending-landstate-assert: could not build a fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$RUN/landstate"

HOME_REPO="$(basename "$REPO")"

# sp-noland: a branch carrying a commit whose content is already on origin/main (squash-merged
# under a different commit), with NO landstate record. This is the sp-qj8n shape: content on
# the base but landing.sh never saw the branch. content_landed returns true for this branch,
# which triggers the assertion path in sending.sh.
git -C "$REPO" checkout -q -b spira/sp-noland
printf 'sp-noland content\n' > "$REPO/sp-noland.txt"
git -C "$REPO" add sp-noland.txt && git -C "$REPO" commit -q -m "sp-noland: add content"
git -C "$REPO" checkout -q main
printf 'sp-noland content\n' > "$REPO/sp-noland.txt"
git -C "$REPO" add sp-noland.txt && git -C "$REPO" commit -q -m "squash sp-noland"

# sp-haslot: same squash-merged shape, but WITH a landstate record (the normal path:
# landing.sh processed this branch and wrote the record before the Sending cleaned it up).
git -C "$REPO" checkout -q -b spira/sp-haslot
printf 'sp-haslot content\n' > "$REPO/sp-haslot.txt"
git -C "$REPO" add sp-haslot.txt && git -C "$REPO" commit -q -m "sp-haslot: add content"
git -C "$REPO" checkout -q main
printf 'sp-haslot content\n' > "$REPO/sp-haslot.txt"
git -C "$REPO" add sp-haslot.txt && git -C "$REPO" commit -q -m "squash sp-haslot"

git -C "$REPO" push -q origin main

printf 'LANDED %s %s %s\n' \
    "$(git -C "$REPO" rev-parse spira/sp-haslot)" \
    "$(date +%s)" fixture-repo > "$RUN/landstate/sp-haslot"

git -C "$REPO" fetch -q origin

printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$HOME_REPO" "$REPO" push main '' '' > "$TMP/repo-map"

B() { bd -C "$SPIRA_DB" "$@"; }
testdb_reset
testdb_seed <<JSONL
{"id":"sp-noland","title":"no landstate record","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-09T00:00:00Z"}
{"id":"sp-haslot","title":"has landstate record","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-09T00:00:00Z"}
JSONL

sending() {
    SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_REPO_MAP="$TMP/repo-map" SPIRA_CONF="$TMP/no-such-conf" \
        bash "$HERE/sending.sh" --no-fetch 2>&1
}

echo "test-sending-landstate-assert.sh"
echo
out="$(sending)"

echo "sp-noland — content on base, no landstate record (sp-qj8n shape):"
want   "assertion fires for sp-noland"       "ASSERT sp-noland"     "$out"
want   "sp-noland was still sent"            "SENT sp-noland"       "$out"

echo
echo "sp-haslot — content on base, landstate record present (normal path):"
nowant "no assertion for sp-haslot"          "ASSERT sp-haslot"     "$out"
want   "sp-haslot was still sent"            "SENT sp-haslot"       "$out"

[ "$fail" -eq 0 ] || { echo; echo "--- sending output ---"; echo "$out"; }
echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
