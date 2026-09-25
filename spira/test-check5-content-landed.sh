#!/usr/bin/env bash
#
# test-check5-content-landed.sh — Sending followed by CHECK 5 recognises content-landed state.
#
# WHAT THIS TESTS. When a bead's diff is already on the base branch:
#   1. The Sending applies the `content-landed` label and reaps the branch.
#   2. Sentinel CHECK 5 does NOT subsequently reopen the bead.
#
# THE DEFECT THIS CATCHES. Two passes used different definitions of "landed": the Sending
# used content_landed (does merging this branch change the base tree?), while CHECK 5
# searched for a commit on the base naming the bead id. A content reap produces no merge
# commit, so CHECK 5 reopened beads the Sending had just correctly reaped — the bead was
# re-worked from scratch, reaped again, and reopened again in a tight loop.
#
# The fix: the Sending labels content-reaped beads `content-landed`; CHECK 5 reads it and
# exempts those beads. This suite proves BOTH halves: the label is applied, the label is
# respected.
#
# THREE CASES (law-absence-needs-a-positive-control):
#   1. POSITIVE CONTROL — a closed bead with no commit IS reopened by CHECK 5 (it is live).
#   2. CONTENT-LANDED — a bead labeled content-landed by the Sending is NOT reopened.
#   3. NORMAL LANDED — a closed bead whose commit IS on the base is also NOT reopened,
#      to confirm the content-landed path does not break the ordinary landing path.
#
# covers: spira/sending.sh spira/sentinel.sh spira/lib.sh
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
testdb_require test-check5-content-landed
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT TERM
testdb_up check5cl || { echo "test-check5-content-landed: could not build a fixture database"; exit 1; }

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

# The Sending runs from $HERE (real scripts). The sentinel runs from $SH, where sending.sh
# is stubbed to exit 0 so CHECK 6 does not interfere with the git state during CHECK 5.
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

# Run the real Sending once (no fetch — the remote is local and already fetched).
# SPIRA_HOME points at $HERE so the real lib.sh is found; SPIRA_REPO overrides the home
# repository lookup so repo_root returns the fixture checkout.
sending() {
    SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_CONF="$TMP/no-such-conf" \
        bash "$HERE/sending.sh" 2>&1
}

# Run the sentinel (CHECK 5 and the rest), with sending.sh stubbed so CHECK 6 cannot
# advance branches or alter the git state between our Sending pass and the assertion.
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

labels_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))' 2>/dev/null; }

branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/spira/$1" 2>/dev/null; }

PAST="2026-09-01T00:00:00Z"
echo "test-check5-content-landed.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL — a closed bead with no commit IS reopened by CHECK 5:"
# Without this, an exemption bug is indistinguishable from CHECK 5 not running at all.
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-bare","title":"bare closed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-bare","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-bare.log"
is "sp-bare starts closed" closed "$(status_of sp-bare)"
out="$(sentinel)"
is "sp-bare IS reopened" open "$(status_of sp-bare)"
want "the pass says so" "reopened sp-bare" "$out"

# ======================================================================================
echo
echo "CONTENT-LANDED — Sending labels the bead; CHECK 5 respects the label:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-cl","title":"content landed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-cl","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-cl.log"

# Create a branch with a commit, then apply the same content to main independently.
# After the push, content_landed(REPO, spira/sp-cl, origin/main) returns true: the
# merge-tree of origin/main and spira/sp-cl is identical to origin/main's own tree.
git -C "$REPO" checkout -q -b spira/sp-cl
printf 'content-landed-work\n' > "$REPO/work-cl.txt"
git -C "$REPO" add work-cl.txt
git -C "$REPO" commit -q -m "sp-cl: add work-cl.txt"
git -C "$REPO" checkout -q main
printf 'content-landed-work\n' > "$REPO/work-cl.txt"
git -C "$REPO" add work-cl.txt
git -C "$REPO" commit -q -m "squash: apply sp-cl diff"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

# Run the Sending. It sees content_landed=true, labels the bead content-landed, reaps
# the branch.
send_out="$(sending)"
want "Sending reports SENT" "SENT sp-cl" "$send_out"
lab="$(labels_of sp-cl)"
want "Sending applied content-landed label" "content-landed" "$lab"
if branch_exists sp-cl; then
    bad "Sending deleted the branch" "spira/sp-cl still exists after the Sending"
else
    ok "Sending deleted the branch"
fi

# CHECK 5 must leave the bead closed: it reads the content-landed label and exempts it.
is "sp-cl is still closed before CHECK 5" closed "$(status_of sp-cl)"
out="$(sentinel)"
is "CHECK 5 does NOT reopen sp-cl" closed "$(status_of sp-cl)"
nowant "the pass does not say it was reopened" "reopened sp-cl" "$out"

# ======================================================================================
echo
echo "NORMAL LANDED — a closed bead whose commit is on the base is also NOT reopened:"
# Confirms the content-landed path does not disturb the ordinary commit-on-base path.
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-nm","title":"normal landed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-nm","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-nm.log"
git -C "$REPO" commit -q --allow-empty -m "sp-nm: the work"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

is "sp-nm starts closed" closed "$(status_of sp-nm)"
out="$(sentinel)"
is "CHECK 5 does NOT reopen sp-nm" closed "$(status_of sp-nm)"
nowant "the pass does not say it was reopened" "reopened sp-nm" "$out"

echo
printf 'test-check5-content-landed.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
