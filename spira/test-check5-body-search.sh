#!/usr/bin/env bash
#
# test-check5-body-search.sh — CHECK 5 does not reopen a bead when its landing commit is
# deep in history (beyond any window-based search), and DOES reopen one whose id is only
# mentioned by an unrelated commit — never landed there.
#
# WHY THIS EXISTS. sp-d9x93 fixed a WINDOW DEFECT: the original search used -n 400 (a
# 400-commit window), so a bead whose commit was commit 401 from the tip was invisible and
# CHECK 5 reopened it as closed-without-landing. sp-a9g hit this at commit 401, sp-37q at
# 400. The fix removed the window: git log now walks the full ancestry.
#
# That same fix once widened the match from the subject line to the whole commit body, on
# the theory that a landing record could name a bead there instead. It could not, in
# practice: sp-dgaig traced five certified branches recorded LANDED and reaped though none
# of their content had reached base, because landed() treated any commit that MENTIONED an
# id — a dependency list, a "Fixes: <id> (analysis)" cross-reference, "Filed <id>" — as a
# landing of that id. landed() now trusts only two subject shapes: the queue's own merge
# subject ("spira: land <id>") or an aeon's own commit for its own bead ("<id>: ..."),
# never a body substring. This suite's MENTION-ONLY case is that regression test.
#
# CASES (law-absence-needs-a-positive-control):
#   1. POSITIVE CONTROL — a closed bead with no commit IS reopened (proves CHECK 5 fires).
#   2. MENTION-ONLY — the bead id appears only in another commit's body, not as a landing
#      record; the bead IS reopened (a mention is not a landing — sp-dgaig).
#   3. DEEP HISTORY — the bead's own landing commit is 401+ commits back from HEAD; NOT
#      reopened. With the old 400-commit window this bead would have been falsely reopened.
#
# defect: sp-d9x93, sp-dgaig
# covers: spira/sentinel.sh spira/lib.sh
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check5-body-search
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT TERM
testdb_up check5bodysearch || { echo "test-check5-body-search: could not build a fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$SH/chamber"

cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'exit 0'
stub strand.sh     'exit 0'
stub sending.sh    'exit 0'
stub reflect.sh    'exit 0'
stub ask.sh        'true'
printf 'FAYTH_LABELS="spira,${SPIRA_PLAN_LABEL}"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' \
    > "$SH/chamber/t.fayth"

HOME_REPO="$(basename "$REPO")"
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$HOME_REPO" "$REPO" pr main '' '' > "$TMP/repo-map"

B() { bd -C "$SPIRA_DB" "$@"; }
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/launch"; chmod +x "$TMP/launch"
printf '#!/usr/bin/env bash\nprintf %%s\\\\n inactive\n' > "$TMP/systemctl"; chmod +x "$TMP/systemctl"

sentinel() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="t" SPIRA_INFERENCE_EVERY=999999 \
    SPIRA_NOTIFY="$SH/ask.sh" SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_CONF="$TMP/no-such-conf" \
        bash "$SH/sentinel.sh" 2>&1
}

status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

PAST="2026-09-01T00:00:00Z"
echo "test-check5-body-search.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL — a closed bead with no commit IS reopened:"
# Without this, every exemption below is indistinguishable from CHECK 5 never firing.
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-ctrl","title":"control","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-ctrl","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-ctrl.log"
is "sp-ctrl starts closed" closed "$(status_of sp-ctrl)"
out="$(sentinel)"
is "sp-ctrl IS reopened" open "$(status_of sp-ctrl)"
want "the pass says so" "reopened sp-ctrl" "$out"

# ======================================================================================
echo
echo "MENTION-ONLY — bead id named by an unrelated commit's body, not a landing record:"
# SEEN RED WITHOUT THE FIX. Before sp-dgaig, landed() matched this exact shape — a
# dependency-list / "Fixes:" style mention in another commit's body — and CHECK 5 would
# NOT reopen sp-mention below. That is the defect: the bead's own work never landed.
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-mention","title":"mentioned but not landed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-mention","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-mention.log"

# An unrelated commit's subject names ITS OWN bead; the body only mentions sp-mention in
# passing, the exact shape that stranded sp-dgaig's five branches.
git -C "$REPO" commit -q --allow-empty -m "$(printf 'sp-other: unrelated work\n\nFixes: sp-mention (root cause analysis)\nThis commit does not carry sp-mentions own changes.')"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

is "sp-mention starts closed" closed "$(status_of sp-mention)"
out="$(sentinel)"
is "mention-only IS reopened" open "$(status_of sp-mention)"
want "the pass says so" "reopened sp-mention" "$out"

# ======================================================================================
echo
echo "DEEP HISTORY — bead commit is 401 commits back from HEAD:"
# Proves CHECK 5 has no window. The old -n 400 window would miss this commit and
# reopen the bead as closed-without-landing.
# 401 empty commits are pushed before the check; the landed commit was the first one.
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-boundary","title":"boundary","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-boundary","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-boundary.log"

# The bead's landing commit is made first; then 401 padding commits push it beyond the
# old 400-commit window from the tip. The check must still find it.
git -C "$REPO" commit -q --allow-empty -m "sp-boundary: the work"
for i in $(seq 1 401); do
    git -C "$REPO" commit -q --allow-empty -m "pad $i"
done
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

is "sp-boundary starts closed" closed "$(status_of sp-boundary)"
out="$(sentinel)"
is "deep-history NOT reopened" closed "$(status_of sp-boundary)"
nowant "the pass does not say reopened" "reopened sp-boundary" "$out"

echo
printf 'test-check5-body-search.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
