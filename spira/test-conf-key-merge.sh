#!/usr/bin/env bash
# test-conf-key-merge.sh — sp-wjkj6: SPIRA_CONF_KEYS is one key per line so that two
# branches which each add a distinct key merge cleanly. Before this bead the allowlist was
# grouped, several keys to a line, and any two feature branches that each appended a key to
# the SAME group line conflicted textually even though the edits touched unrelated keys —
# conf.sh had 124 commits in 3 days and was the queue's conflict hot spot because of exactly
# this shape.
#
# THE CONTROL COMES FIRST (law-absence-needs-a-positive-control): reconstruct the grouped,
# several-keys-per-line shape the allowlist used to have, make the same two-branch edit, and
# require git to see a CONFLICT there. Only then does the one-per-line fixture's clean merge
# mean anything.
#
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-conf-key-merge.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# repo_with <path> <content> -> a fresh git repo at <path> whose conf-keys.txt is <content>,
# on branch "base".
repo_with() {
    local repo="$1" content="$2"
    rm -rf "$repo"; mkdir -p "$repo"
    ( cd "$repo" && git init -q -b base \
        && printf '%s' "$content" > conf-keys.txt \
        && git add conf-keys.txt && git commit -q -m base ) >/dev/null
}

# add_line_and_commit <repo> <branch> <sed-expr> <msg> -> checks out a fresh branch from
# base and applies a sed expression against conf-keys.txt, committing the result.
add_and_commit() {
    local repo="$1" branch="$2" expr="$3" msg="$4"
    ( cd "$repo" && git checkout -q -b "$branch" base \
        && sed -i "$expr" conf-keys.txt \
        && git commit -q -am "$msg" ) >/dev/null
}

# =============================================================================
echo
echo "control: today's grouped (several-keys-per-line) shape CONFLICTS:"
# =============================================================================
# Reproduces the shape SPIRA_CONF_KEYS had before this bead: several keys sharing one
# line. Two branches each append a distinct key to that SAME line — the independent-edit
# case the bead's evidence names (sp-zs04v.3/.4, sp-o9nkc, sp-umcjk, sp-xsl8i, sp-5dcpj).
GROUPED='SPIRA_HOME_REPO SPIRA_DB SPIRA_RUN SPIRA_GOAL
SPIRA_PATH SPIRA_WORKSPACES SPIRA_REPO_MAP
SPIRA_MAIL SPIRA_MAIL_KINDS'
OLD="$TMP/old-repo"
repo_with "$OLD" "$GROUPED"
add_and_commit "$OLD" alpha \
    's/^SPIRA_PATH SPIRA_WORKSPACES SPIRA_REPO_MAP$/SPIRA_PATH SPIRA_WORKSPACES SPIRA_REPO_MAP SPIRA_ZZTEST_ALPHA/' \
    "add SPIRA_ZZTEST_ALPHA"
add_and_commit "$OLD" bravo \
    's/^SPIRA_PATH SPIRA_WORKSPACES SPIRA_REPO_MAP$/SPIRA_PATH SPIRA_WORKSPACES SPIRA_REPO_MAP SPIRA_ZZTEST_BRAVO/' \
    "add SPIRA_ZZTEST_BRAVO"
( cd "$OLD" && git checkout -q alpha && git merge -q --no-edit bravo ) >"$TMP/old-merge.log" 2>&1
old_rc=$?
if [ "$old_rc" -ne 0 ] && grep -q '^<<<<<<<' "$OLD/conf-keys.txt" 2>/dev/null; then
    ok "grouped form conflicts on two independent key insertions"
else
    bad "grouped form conflicts on two independent key insertions" \
        "rc=$old_rc, expected a merge conflict; this control must fail before the fix means anything: $(cat "$TMP/old-merge.log")"
fi
( cd "$OLD" && git merge --abort ) >/dev/null 2>&1

# =============================================================================
echo
echo "fix: one key per line merges cleanly and both keys resolve:"
# =============================================================================
ONE_PER_LINE='SPIRA_DB
SPIRA_GOAL
SPIRA_HOME_REPO
SPIRA_MAIL
SPIRA_MAIL_KINDS
SPIRA_PATH
SPIRA_REPO_MAP
SPIRA_RUN
SPIRA_WORKSPACES'
NEW="$TMP/new-repo"
repo_with "$NEW" "$ONE_PER_LINE"
# Sorted insertion, mirroring how a real branch would add a key to conf.sh's sorted list.
add_and_commit "$NEW" alpha 's/^SPIRA_WORKSPACES$/SPIRA_WORKSPACES\nSPIRA_ZZTEST_ALPHA/' \
    "add SPIRA_ZZTEST_ALPHA"
add_and_commit "$NEW" bravo 's/^SPIRA_GOAL$/SPIRA_GOAL\nSPIRA_ZZTEST_BRAVO/' \
    "add SPIRA_ZZTEST_BRAVO"
( cd "$NEW" && git checkout -q alpha && git merge -q --no-edit bravo ) >"$TMP/new-merge.log" 2>&1
new_rc=$?
wantrc "one-per-line form merges cleanly" 0 "$new_rc"
[ "$new_rc" -ne 0 ] && cat "$TMP/new-merge.log" >&2

MERGED="$(cat "$NEW/conf-keys.txt" 2>/dev/null)"
want "merged file: SPIRA_ZZTEST_ALPHA present" "SPIRA_ZZTEST_ALPHA" "$MERGED"
want "merged file: SPIRA_ZZTEST_BRAVO present" "SPIRA_ZZTEST_BRAVO" "$MERGED"

# =============================================================================
echo
echo "both keys resolve through conf.sh's own membership test once merged:"
# =============================================================================
# The flatten step conf.sh applies to its real SPIRA_CONF_KEYS ($(echo $VAR), which
# word-splits on any whitespace including newlines) collapses this merged, one-per-line
# text into the same space-padded single line the " $KEY " case test already reads.
SPIRA_CONF_KEYS="$MERGED"
FLATTENED=" $(echo $SPIRA_CONF_KEYS) "
for want_key in SPIRA_ZZTEST_ALPHA SPIRA_ZZTEST_BRAVO; do
    case "$FLATTENED" in
        *" $want_key "*) ok "$want_key is a member after merge" ;;
        *) bad "$want_key is a member after merge" "not found in flattened allowlist: $FLATTENED" ;;
    esac
done

tl_summary
