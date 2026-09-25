#!/usr/bin/env bash
#
# test-landing-base-fail.sh — when a repository's own base fails the gate, the pass holds
# every pending branch rather than reopening it. ONE incident is filed however many branches
# are behind the red and however many passes go by. The held branch lands once the base is
# green.
#
# Extracted from test-landing.sh to reduce the critical-path suite time.
#
# covers: spira/landing.sh spira/lib.sh spira/incident.sh
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-base-fail
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing-base-fail || {
    printf 'SKIP test-landing-base-fail: testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
REPONAME=fixture-repo
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub mail.sh '[ "${1:-}" = send ] || exit 0; printf "%s\n" "$*" >> "$EMITTED"; cat >> "$EMITTED"; printf "\n" >> "$EMITTED"'
export EMITTED="$TMP/events"; : > "$EMITTED"
stub gh 'exit 1'

stub gate.sh '
r="$SPIRA_RUN/reap-during-gate"
if [ -s "$r" ]; then
    while read -r id; do
        [ -n "$id" ] || continue
        git -C "'"$REPO"'" worktree remove --force "'"$RUN"'/worktree/$id" >/dev/null 2>&1
        git -C "'"$REPO"'" branch -D "spira/$id" >/dev/null 2>&1
    done < "$r"
    : > "$r"
fi
mkdir -p "$SPIRA_RUN/tip-at-gate"
git -C "'"$REPO"'" rev-parse "$1" > "$SPIRA_RUN/tip-at-gate/${1//\//-}" 2>/dev/null
w="$SPIRA_RUN/withhold-gate"
if [ -s "$w" ] && grep -qx "$1" "$w"; then
    echo "gate: VERDICT=NO_VERDICT reason=stub-busy branch=$1 repo=${2:-?}" >&2
    exit "${SPIRA_GATE_NOVERDICT:?the gate protocol constant is not in the environment}"
fi
c="$SPIRA_RUN/claim-during-gate"
if [ -s "$c" ]; then
    while read -r id pid; do
        [ -n "$id" ] || continue
        printf "%s\\n" "$pid" > "$SPIRA_RUN/aeon-builder-$id.pid"
    done < "$c"
    : > "$c"
fi
echo "gate: VERDICT=PASS reason=${GATE_REASON:-stub} branch=$1 repo=${2:-?}" >&2; exit 0'

cp "$SH/gate.sh" "$TMP/gate-full.sh"

cat > "$SH/repo-map" <<MAP
$REPONAME | $REPO | push | |
MAP

B() { bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }
notes_of() { B show "$1" 2>/dev/null; }

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
        bash "$SH/landing.sh" 2>&1
}

seed() {
    testdb_reset
    rm -rf "$RUN/tip-at-gate"; rm -f "$RUN/withhold-gate" "$RUN/claim-during-gate"
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

branch() {
    local id="$1" f="${2:-$1.txt}" c="${3:-$1}"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$c" > "$RUN/worktree/$id/$f"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}

drop_branch() {
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1
}

echo "test-landing-base-fail.sh"

# --------------------------------------------------------------------------------------
# A BASE THAT FAILS ITS OWN GATE: THE BRANCH IS HELD, THE REPOSITORY IS CHARGED
#
# The gate has said "it fails against the base too — this branch did not cause it" since
# BASE_FAIL existed, and the pass already declines to reopen on it. What it did with that
# sentence afterwards was nothing: a log line saying the next pass would take it, said again
# every two minutes, while every branch of the repository sat behind a red nobody owned.
#
# So three properties, and the third is what makes the first two worth anything. The branch
# is held rather than reopened; ONE incident is filed however many branches are behind the
# same red and however many passes go by; and the held branch lands on the pass after the
# base is green, which is the whole reason holding is the right answer rather than refusing.
# --------------------------------------------------------------------------------------
echo
BASE_SUITE=test-fx-base.sh
stub gate.sh '
echo "gate: the fixture repository gate failed: '"$BASE_SUITE"' FAILED" >&2
echo "gate: it fails against origin/main too — this branch did not cause it." >&2
echo "gate: VERDICT=BASE_FAIL reason=base-red branch=$1 repo=${2:-?} suite='"$BASE_SUITE"'" >&2
exit 76'

incidents() {
    B list --status open,in_progress --limit 0 --label "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan},repo:$REPONAME" --json 2>/dev/null \
      | python3 -c '
import json, sys
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
for i in (d if isinstance(d, list) else [d]): print(i["id"])'
}
n_lines() { printf '%s' "$1" | grep -c . || true; }

seed; branch sp-held; branch sp-heldtoo
out="$(landing)"
want   "a base that fails its own gate holds the branch" \
       "gate: held — the base fails its own gate" "$out"
want   "and the hold names the suite the gate named"     "suite $BASE_SUITE" "$out"
nowant "the bead is not reopened"                        "reopened sp-held" "$out"
is     "and stays closed"                                closed "$(status_of sp-held)"
nowant "nor is the second branch behind the same red"    "reopened sp-heldtoo" "$out"
nowant "and nothing is landed on a withheld verdict"     "landed spira/sp-held" "$out"

inc="$(incidents)"
is "one incident is filed for the repository" 1 "$(n_lines "$inc")"
inc_id="$(printf '%s\n' "$inc" | head -1)"
if [ -n "$inc_id" ]; then
    shown="$(B show "$inc_id" 2>&1)"
    want "it names the failing suite"                  "$BASE_SUITE" "$shown"
    want "and the repository whose base is red"        "$REPONAME" "$shown"
    want "and says no bead was reopened or charged"    "no attempt charged" "$shown"
    want "and carries the gate's own output"           "this branch did not cause it" "$shown"
    labels="$(B label list "$inc_id" 2>&1)"
    want "it lands in the builders partition"          "plan" "$labels"
    want "labelled with the repository"                "repo:$REPONAME" "$labels"
fi

out2="$(landing)"
is   "a second pass files no second incident"  1 "$(n_lines "$(incidents)")"
is   "it records one recurrence in the incident log" "1" \
     "$(grep -c ' recurred ' "$RUN/incident.log" 2>/dev/null || echo 0)"
want "and holds the branch again"              "gate: held — the base fails its own gate" "$out2"
is   "with the bead still closed"              closed "$(status_of sp-held)"

stub gate.sh 'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?} suite=-" >&2; exit 0'
out3="$(landing)"
want "a held branch lands on the pass after the base is green" "landed spira/sp-held" "$out3"
want "and so does the one behind it"                           "landed spira/sp-heldtoo" "$out3"
drop_branch sp-held; drop_branch sp-heldtoo

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
