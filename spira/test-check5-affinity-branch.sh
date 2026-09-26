#!/usr/bin/env bash
#
# test-check5-affinity-branch.sh — CHECK 5 reads the bead's RECORDED branch affinity, not
# only the id-derived refs/heads/spira/<id>, before deciding a closed bead was never landed.
#
# THE DEFECT THIS CATCHES. Sibling subtask beads normally share one branch — the bead's
# recorded affinity (bead_branch, lib.sh — law-branch-affinity-is-recorded) — rather than
# each getting its own refs/heads/spira/<id>. CHECK 5 walked the base branch and then, for
# beads it couldn't find there, checked only the literal refs/heads/spira/<id>. That ref
# never exists for a sibling sharing a parent's branch, so a bead whose commit landed
# correctly on the shared branch was reopened as "closed without landing" even though the
# commit matched CHECK 5's own subject rule (^<id>:) exactly.
#
# THREE CASES (law-absence-needs-a-positive-control):
#   1. POSITIVE CONTROL — a closed bead with no commit anywhere IS reopened (the check is live).
#   2. AFFINITY BRANCH — a closed bead whose commit is on its RECORDED affinity branch (not
#      its own id-derived branch, which never exists) is NOT reopened, and the log names the
#      affinity branch that satisfied the check.
#   3. NO MATCHING COMMIT — a closed bead recorded against a live affinity branch that carries
#      no commit naming it (someone else's work) IS still reopened — the affinity check does
#      not blanket-exempt every bead recorded against a branch that merely exists.
#
# covers: spira/sentinel.sh spira/lib.sh
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check5-affinity-branch
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT TERM
testdb_up check5ab || { echo "test-check5-affinity-branch: could not build a fixture database"; exit 1; }

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

cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'exit 0'
stub strand.sh     'exit 0'
stub sending.sh    'exit 0'
stub reflect.sh    'exit 0'
stub ask.sh        'true'
printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' \
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
echo "test-check5-affinity-branch.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL — a closed bead with no commit anywhere IS reopened by CHECK 5:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-bare","title":"bare closed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-bare","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-bare.log"
out="$(sentinel)"
is "sp-bare IS reopened" open "$(status_of sp-bare)"
want "the pass says so" "reopened sp-bare" "$out"

# ======================================================================================
echo
echo "AFFINITY BRANCH — a commit on the RECORDED (shared) branch is not reopened:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-par.1","title":"sibling subtask","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-par.1","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-par.1.log"
B set-state sp-par.1 "branch=spira/sp-par" >/dev/null 2>&1

# The bead's OWN id-derived branch never exists — the normal shape for a subtask sharing
# its parent's branch. The commit lands only on the shared affinity branch.
git -C "$REPO" checkout -q -b spira/sp-par
printf 'sibling-work\n' > "$REPO/work-par1.txt"
git -C "$REPO" add work-par1.txt
git -C "$REPO" commit -q -m "sp-par.1: sibling did its part"
git -C "$REPO" checkout -q main

# Positive control demanded by law-absence-needs-a-positive-control: the commit really is
# findable on the affinity branch (the search CHECK 5 must widen to), and really is absent
# from the base (the search CHECK 5 already performs, and would reopen on alone).
aff_hits="$(git -C "$REPO" log --format='%s' spira/sp-par | grep -c '^sp-par\.1:')"
base_hits="$(git -C "$REPO" log --format='%s' origin/main | grep -c '^sp-par\.1:')"
[ "${aff_hits:-0}" -ge 1 ] && ok "positive control: commit is on the affinity branch" \
    || bad "positive control: commit is on the affinity branch" "found $aff_hits"
is "positive control: commit is absent from the base" 0 "$base_hits"
if git -C "$REPO" show-ref --verify -q refs/heads/spira/sp-par.1; then
    bad "positive control: bead's own id-derived branch does not exist" "it does"
else
    ok "positive control: bead's own id-derived branch does not exist"
fi

out="$(sentinel)"
is "CHECK 5 does NOT reopen sp-par.1" closed "$(status_of sp-par.1)"
nowant "the pass does not say it was reopened" "reopened sp-par.1" "$out"
want "the log names the affinity branch that satisfied the check" "commit on affinity branch spira/sp-par matches" "$out"

# ======================================================================================
echo
echo "NO MATCHING COMMIT — recorded against a live branch with no commit naming it IS reopened:"
# The affinity check must not blanket-exempt every bead recorded against an existing branch.
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-par.2","title":"unrelated subtask","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-par.2","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-par.2.log"
B set-state sp-par.2 "branch=spira/sp-par" >/dev/null 2>&1

out="$(sentinel)"
is "CHECK 5 reopens sp-par.2 (no commit names it anywhere)" open "$(status_of sp-par.2)"
want "the pass says so" "reopened sp-par.2" "$out"

tl_summary
