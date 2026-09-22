#!/usr/bin/env bash
#
# test-sending-content-label.sh — the Sending labels a content reap, and ONLY a content reap.
#
#   ./test-sending-content-label.sh
#
# WHY THIS EXISTS. The write half of sp-796o. CHECK 5 skips a bead carrying `content-landed`,
# and test-content-landed-no-reopen.sh proves that half — but a label nothing ever writes
# makes that exemption dead code, and the loop goes on running while both halves look correct
# in isolation. The first attempt at this fix shipped a read guard that could never match; the
# way to not ship its mirror image is to assert the write.
#
# THE NARROWING IS THE POINT. content_landed is true in two different situations and only one
# of them means the work is on the base:
#
#   * a branch with commits of its own whose diff is already on the base — merging changes
#     nothing because someone else landed the same change. THE LABEL BELONGS HERE.
#   * a branch with no commits at all, which an aeon leaves when it dies before committing.
#     Merging changes nothing because there is nothing to merge. Labelling this would exempt
#     empty work from the one check that catches it, so THE LABEL MUST NOT BE WRITTEN.
#
# That second case is not hypothetical: killing three aeons mid-work on 2026-09-08 left three
# such branches within the minute, and the reflog of one showed a single entry — "Created from
# origin/main".
#
# defect: sp-796o
# covers: spira/sending.sh
# hermetic-ok: fixture database and local git repos, no systemd, no network
# scar: the content-landed label was never written; the CHECK 5 exemption that skips labelled beads was dead code, and the loop ran on every landing regardless.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sending-content-label
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sendcontentlabel || { echo "test-sending-content-label: could not build a fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree"

# sp-cont — a branch carrying a commit whose diff is ALREADY on the base. Built by making the
# identical change twice: once on the branch, once on the base under a different commit. That
# is a squash landing under someone else's id, which is the case the label describes.
git -C "$REPO" checkout -q -b spira/sp-cont
printf 'same content\n' > "$REPO/shared.txt"
git -C "$REPO" add shared.txt && git -C "$REPO" commit -q -m "sp-cont: add shared.txt"
git -C "$REPO" checkout -q main
printf 'same content\n' > "$REPO/shared.txt"
git -C "$REPO" add shared.txt && git -C "$REPO" commit -q -m "someone else landed the same change"
git -C "$REPO" push -q origin main

# sp-empty — a branch with no commits of its own, as a killed aeon leaves it.
git -C "$REPO" branch spira/sp-empty main

git -C "$REPO" fetch -q origin

HOME_REPO="$(basename "$REPO")"
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$HOME_REPO" "$REPO" pr main '' '' > "$TMP/repo-map"

B() { bd -C "$SPIRA_DB" "$@"; }
testdb_reset
testdb_seed <<JSONL
{"id":"sp-cont","title":"content already on base","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-08T00:00:00Z"}
{"id":"sp-empty","title":"aeon died before committing","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-08T00:00:00Z"}
JSONL

sending() {
    SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_REPO_MAP="$TMP/repo-map" SPIRA_CONF="$TMP/no-such-conf" \
        bash "$HERE/sending.sh" --no-fetch 2>&1
}

labels_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(",".join(d[0].get("labels") or []))'; }
has() { case ",$(labels_of "$1")," in *",$2,"*) echo yes ;; *) echo no ;; esac; }

echo "test-sending-content-label.sh"
echo
out="$(sending)"

echo "the branch whose diff is already on the base:"
is "sp-cont was sent"                  "yes" "$(case "$out" in *"SENT sp-cont"*) echo yes;; *) echo no;; esac)"
is "and carries content-landed"        "yes" "$(has sp-cont content-landed)"

echo
echo "the branch with no commits of its own:"
# sp-bf31a: content_landed returns 0 for ANY ancestor of the base, including an empty
# branch.  The Sending reaps it (no content-landed label — _ahead=0).  The label is
# what would exempt the bead from CHECK 5's closed-not-landed reopen; the branch
# deletion alone does not.  CHECK 5 finds no commit on the base naming sp-empty and
# reopens it via the normal path (sp-qc4kn).
is "sp-empty WAS sent (reaped as ancestor, CHECK 5 reopens)" \
   "yes" "$(case "$out" in *"SENT sp-empty"*) echo yes;; *) echo no;; esac)"
is "and is NOT labelled content-landed" "no"  "$(has sp-empty content-landed)"

[ "$fail" -eq 0 ] || { echo; echo "--- sending output ---"; echo "$out"; }
echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
