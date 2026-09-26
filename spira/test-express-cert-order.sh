#!/usr/bin/env bash
# test-express-cert-order.sh — express lane cert order: express branch certified before non-express.
#
# Acceptance (sp-fnx1p):
#   With a base-fix branch, an express branch and ordinary branches, the order is
#   base-fix, express, then ordinary — regardless of refname.
#   Oldest-closed-first still holds within each group.
#
# Positive control (law-a-regression-test-must-be-seen-to-fail): sp-b-express sorts
# after sp-a-regular alphabetically; without the express-first reorder the test fails.
#
# covers: spira/landing.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-express-cert-order
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up express-cert-order || {
    printf 'SKIP test-express-cert-order: testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/chamber" "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub gate.sh '
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n" "$1" "${2:-?}" >&2
exit 0'
stub gh 'exit 1'

plant_bead_with_labels() {
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":%s,"updated_at":"2026-09-04T00:00:00Z","closed_at":"%s","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$1" "$1" "$2" "${3:-2026-09-04T00:00:00Z}" "$1" | testdb_seed
}

landing_run() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" \
        bash "$SH/landing.sh" 2>&1
}

make_branch() {
    local bid="$1" file="$2"
    local wt="$RUN/worktree/$bid"
    git -C "$REPO" worktree add -q -b "spira/$bid" "$wt" main
    printf '%s\n' "$file" > "$wt/$bid.txt"
    git -C "$wt" add -A
    git -C "$wt" commit -q -m "$bid: work"
}

echo "test-express-cert-order.sh"

# ======================================================================================
echo
echo "landing: express branch certified before non-express (refname-adversarial)"
# ======================================================================================
# POSITIVE CONTROL: sp-b-express sorts AFTER sp-a-regular alphabetically.
# Without the express-first reorder, sp-a-regular would be certified first and the
# test would fail — proving the test is sensitive to the code change.
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED

plant_bead_with_labels "sp-a-regular" '["spira","plan","repo:fixture-repo"]'
plant_bead_with_labels "sp-b-express" '["spira","plan","repo:fixture-repo","express"]'

make_branch "sp-a-regular" "regular"
make_branch "sp-b-express" "express"

out="$(landing_run)"

# express-first log line must mention the branch
want "landing: express-first log reports express branch" \
    "express branch" "$out"
want "landing: express-first log names sp-b-express" \
    "spira/sp-b-express" "$out"

# sp-b-express must be certified before sp-a-regular in the output
pos_express="$(printf '%s' "$out" | grep -n "certified spira/sp-b-express" | cut -d: -f1 | head -1)"
pos_regular="$(printf '%s' "$out" | grep -n "certified spira/sp-a-regular" | cut -d: -f1 | head -1)"
if [ -n "$pos_express" ] && [ -n "$pos_regular" ]; then
    [ "$pos_express" -lt "$pos_regular" ] \
        && ok "landing: express certified before non-express (lines $pos_express < $pos_regular)" \
        || bad "landing: express certified before non-express" \
               "express line $pos_express, regular line $pos_regular"
else
    bad "landing: could not find both certification lines" \
        "express=$pos_express regular=$pos_regular out=$out"
fi

# ======================================================================================
echo
echo "landing: base-fix first, then express, then ordinary"
# ======================================================================================
# Three beads: sp-a-basefail (base-fix), sp-b-express (express), sp-c-ordinary.
# Refname order would give: sp-a, sp-b, sp-c — base-fix coincidentally first here,
# but the express ordering must still hold between sp-b and sp-c.
rm -rf "$RUN/worktree/sp-a-regular" "$RUN/worktree/sp-b-express"
rm -f "$LANDSTATE"/sp-a-regular "$LANDSTATE"/sp-b-express
git -C "$REPO" worktree prune 2>/dev/null || true
git -C "$REPO" branch -D spira/sp-a-regular spira/sp-b-express 2>/dev/null || true

testdb_reset
testdb_seed <<'SEED'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED

# sp-a-basefail: external_ref=basefail:fixture-repo:test-x
printf '{"id":"sp-a-basefail","title":"sp-a-basefail","status":"closed","issue_type":"task","labels":["spira","plan","repo:fixture-repo"],"external_ref":"basefail:fixture-repo:test-x","updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-a-basefail","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
    | testdb_seed
plant_bead_with_labels "sp-b-express" '["spira","plan","repo:fixture-repo","express"]'
plant_bead_with_labels "sp-c-ordinary" '["spira","plan","repo:fixture-repo"]'

make_branch "sp-a-basefail" "basefail"
make_branch "sp-b-express" "express"
make_branch "sp-c-ordinary" "ordinary"

out2="$(landing_run)"

pos_bf="$(printf '%s' "$out2" | grep -n "certified spira/sp-a-basefail" | cut -d: -f1 | head -1)"
pos_ex="$(printf '%s' "$out2" | grep -n "certified spira/sp-b-express" | cut -d: -f1 | head -1)"
pos_ord="$(printf '%s' "$out2" | grep -n "certified spira/sp-c-ordinary" | cut -d: -f1 | head -1)"

if [ -n "$pos_bf" ] && [ -n "$pos_ex" ] && [ -n "$pos_ord" ]; then
    [ "$pos_bf" -lt "$pos_ex" ] && [ "$pos_ex" -lt "$pos_ord" ] \
        && ok "landing: base-fix < express < ordinary (lines $pos_bf < $pos_ex < $pos_ord)" \
        || bad "landing: base-fix < express < ordinary" \
               "basefail=$pos_bf express=$pos_ex ordinary=$pos_ord"
else
    bad "landing: could not find all three certification lines" \
        "basefail=$pos_bf express=$pos_ex ordinary=$pos_ord"
fi

# ======================================================================================
echo
echo "landing: oldest-closed first within express group"
# ======================================================================================
# Two express beads with different closed_at; the older one must be certified first.
rm -rf "$RUN/worktree/sp-a-basefail" "$RUN/worktree/sp-b-express" "$RUN/worktree/sp-c-ordinary"
rm -f "$LANDSTATE"/sp-a-basefail "$LANDSTATE"/sp-b-express "$LANDSTATE"/sp-c-ordinary
git -C "$REPO" worktree prune 2>/dev/null || true
git -C "$REPO" branch -D spira/sp-a-basefail spira/sp-b-express spira/sp-c-ordinary 2>/dev/null || true

testdb_reset
testdb_seed <<'SEED'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED

# sp-z-newer closed after sp-a-older; refname puts sp-z after sp-a (both express)
plant_bead_with_labels "sp-a-older" '["spira","plan","repo:fixture-repo","express"]' "2026-09-01T00:00:00Z"
plant_bead_with_labels "sp-z-newer" '["spira","plan","repo:fixture-repo","express"]' "2026-09-03T00:00:00Z"

make_branch "sp-a-older" "older"
make_branch "sp-z-newer" "newer"

out3="$(landing_run)"

pos_older="$(printf '%s' "$out3" | grep -n "certified spira/sp-a-older" | cut -d: -f1 | head -1)"
pos_newer="$(printf '%s' "$out3" | grep -n "certified spira/sp-z-newer" | cut -d: -f1 | head -1)"
if [ -n "$pos_older" ] && [ -n "$pos_newer" ]; then
    [ "$pos_older" -lt "$pos_newer" ] \
        && ok "landing: older express certified before newer express (lines $pos_older < $pos_newer)" \
        || bad "landing: older express certified before newer express" \
               "older=$pos_older newer=$pos_newer"
else
    bad "landing: could not find both express certification lines" \
        "older=$pos_older newer=$pos_newer"
fi

# ======================================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
