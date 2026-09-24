#!/usr/bin/env bash
#
# test-check5-landed-assert.sh — a bead whose commit is an ancestor of the base is never
#   reopened across repeated sentinel passes, and no attempt label is charged.
#
# WHY THIS EXISTS. sp-xrwdg: beads sp-a9g, sp-37q, sp-m0s7, sp-796o had their commits
#   genuinely on the base branch, but CHECK 5's text search failed to find them (window
#   limit or subject-only format) and reopened them, charging attempts. Some were poisoned
#   and reopened again — sp-637b cycled six times because the poison label did not stop
#   the reopen loop, and attempt 6 was charged against work that had landed at attempt 1.
#
# WHAT IS DIFFERENT FROM test-check5-body-search.sh. That suite verifies the SEARCH
#   MECHANISM (deep history walks the full ancestry; body search covers %B). This suite
#   verifies the PROPERTY: a bead genuinely on the base — proven by git ancestry, not by
#   trusting the same text search being tested — is never reopened and never charged,
#   even across two consecutive passes. The second pass catches a latent bug where the
#   first changes bead state in a way that makes the second reopen; the direct ancestry
#   check proves the fixture is correct before the sentinel ever runs.
#
# SEEN TO FAIL AGAINST UNFIXED CODE. The deep-history case pushes 401 commits after the
#   landing commit, placing it beyond the old 400-commit window. Against the unfixed
#   sentinel (which used `git log -n 400 --format='%B'`), pass 1 would reopen the bead
#   and charge attempt 1 — the assertion "pass 1: sp-land stays closed" would fail as:
#     FAIL pass 1: sp-land stays closed: wanted [closed] got [open]
#   and the attempt assertion would follow as:
#     FAIL pass 1: no attempt label charged: wanted [] got [sp-attempt-1]
#   Both were verified against a patched copy before this test was committed (sp-xrwdg).
#
# defect: sp-xrwdg
# covers: spira/sentinel.sh
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
testdb_require test-check5-landed-assert
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT TERM
testdb_up check5lndassert || { echo "test-check5-landed-assert: could not build a fixture database"; exit 1; }

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

status_of()   { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }
attempts_of() { B label list "$1" 2>/dev/null | grep -oE 'sp-attempt-[0-9]+' | head -1 || true; }

PAST="2026-09-01T00:00:00Z"
echo "test-check5-landed-assert.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL — a closed bead with no commit IS reopened (CHECK 5 fires):"
# Without this, every passing case below is indistinguishable from CHECK 5 not running.
# ======================================================================================
testdb_reset
testdb_seed <<JSONL || { echo "seed failed"; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-ctrl","title":"ctrl","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-ctrl","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-ctrl.log"
is "sp-ctrl starts closed" closed "$(status_of sp-ctrl)"
out="$(sentinel)"
is "sp-ctrl IS reopened" open "$(status_of sp-ctrl)"
want "the pass says so" "reopened sp-ctrl" "$out"

# ======================================================================================
echo
echo "LANDED — ancestry-verified commit; bead stays closed across two passes, no attempt:"
# The landing commit is made first; 401 padding commits follow, pushing it past the old
# 400-commit window. This is the configuration that exposed sp-a9g and sp-37q.
# ======================================================================================
testdb_reset
testdb_seed <<JSONL || { echo "seed failed"; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-land","title":"land","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-land","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-land.log"

git -C "$REPO" commit -q --allow-empty -m "sp-land: implement the work"
land_sha="$(git -C "$REPO" rev-parse HEAD)"
for i in $(seq 1 401); do
    git -C "$REPO" commit -q --allow-empty -m "pad $i"
done
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

# VERIFY LANDING BY ANCESTRY. This is the test's independent proof that the commit is
# on the base, not inferred from the same text-search mechanism CHECK 5 uses. If the
# fixture is wrong (the commit is not actually on origin/main), the sentinel passing
# would be a false negative from the test setup, not a check on CHECK 5's behaviour.
if git -C "$REPO" merge-base --is-ancestor "$land_sha" origin/main 2>/dev/null; then
    ok "landing confirmed by ancestry (--is-ancestor)"
else
    bad "landing confirmed" "commit $land_sha is NOT an ancestor of origin/main — fixture is wrong"
fi

is "sp-land starts closed" closed "$(status_of sp-land)"

# PASS 1
out1="$(sentinel)"
is "pass 1: sp-land stays closed" closed "$(status_of sp-land)"
nowant "pass 1: not reopened" "reopened sp-land" "$out1"
att1="$(attempts_of sp-land)"
is "pass 1: no attempt label charged" "" "$att1"

# PASS 2. A bug where the first pass alters bead state in a way that causes the second
# to reopen (e.g., adding a label that shifts a column in the bulk query) would be caught
# here. Against the old 400-commit-window code this is also open(attempt-1) → the pass-2
# assertion fails, because the second pass charges attempt-2 and reopens again.
out2="$(sentinel)"
is "pass 2: sp-land stays closed" closed "$(status_of sp-land)"
nowant "pass 2: not reopened" "reopened sp-land" "$out2"
att2="$(attempts_of sp-land)"
is "pass 2: no attempt label charged" "" "$att2"

echo
printf 'test-check5-landed-assert.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
