#!/usr/bin/env bash
#
# test-check5-hold-no-reopen.sh — CHECK 5 does not reopen a closed bead in a land=hold repo.
#
# In a hold repo, landing.sh leaves the branch standing and does not merge without a
# human decision. A closed bead with a standing branch is the expected terminal state.
# CHECK 5 must not reopen it whether the branch is ahead of the base or not.
#
# THREE CASES (law-absence-needs-a-positive-control):
#   1. POSITIVE CONTROL  — a closed bead in a push-mode repo with no commit IS reopened.
#   2. HOLD, AHEAD > 0   — closed bead in hold repo, branch has commits; NOT reopened.
#   3. HOLD, AHEAD == 0  — closed bead in hold repo, empty branch; NOT reopened.
#
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
testdb_require test-check5-hold-no-reopen
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up holdnoreopen || { echo "test-check5-hold-no-reopen: could not build a fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

PUSH_REPO="$TMP/push-repo"; HOLD_REPO="$TMP/hold-repo"
RUN="$TMP/run"; SH="$TMP/spira"

git init -q -b main "$PUSH_REPO"
git init -q -b main "$HOLD_REPO"
git -C "$PUSH_REPO" commit -q --allow-empty -m base
git -C "$HOLD_REPO" commit -q --allow-empty -m base

mkdir -p "$RUN/worktree" "$SH/chamber"

cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'exit 0'
stub strand.sh     'exit 0'
stub sending.sh    'exit 0'
stub reflect.sh    'exit 0'
stub ask.sh        'true'

printf 'FAYTH_LABELS="spira,${SPIRA_PLAN_LABEL}"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/t.fayth"

PUSH_NAME="$(basename "$PUSH_REPO")"
HOLD_NAME="$(basename "$HOLD_REPO")"
printf '%s | %s | push | main | | \n' "$PUSH_NAME" "$PUSH_REPO" > "$TMP/repo-map"
printf '%s | %s | hold | main | | \n' "$HOLD_NAME" "$HOLD_REPO" >> "$TMP/repo-map"

B() { bd -C "$SPIRA_DB" "$@"; }
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/launch"
printf '#!/usr/bin/env bash\nprintf %%s\\\\n inactive\n' > "$TMP/systemctl"
chmod +x "$TMP/launch" "$TMP/systemctl"

sentinel() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$PUSH_REPO" SPIRA_HOME_REPO="$PUSH_NAME" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="t" SPIRA_INFERENCE_EVERY=999999 \
    SPIRA_NOTIFY="$SH/ask.sh" SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_CONF="$TMP/no-such-conf" \
        bash "$SH/sentinel.sh" 2>&1
}

status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
    print(d[0].get("status") or "")
except Exception:
    pass'; }

echo "test-check5-hold-no-reopen.sh"

seed() {
    testdb_reset || { echo "seed: testdb_reset failed" >&2; exit 1; }
    testdb_seed <<JSONL || { echo "seed: testdb_seed failed" >&2; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-08T00:00:00Z"}
{"id":"sp-push","title":"push-mode closed, no commit","status":"closed","issue_type":"task","labels":["spira","plan","repo:$PUSH_NAME"],"updated_at":"2026-09-08T00:00:00Z","dependencies":[{"issue_id":"sp-push","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-hold-ahead","title":"hold-mode closed, branch ahead","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOLD_NAME"],"updated_at":"2026-09-08T00:00:00Z","dependencies":[{"issue_id":"sp-hold-ahead","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-hold-empty","title":"hold-mode closed, empty branch","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOLD_NAME"],"updated_at":"2026-09-08T00:00:00Z","dependencies":[{"issue_id":"sp-hold-empty","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
    touch "$RUN/sp-push.log" "$RUN/sp-hold-ahead.log" "$RUN/sp-hold-empty.log"

    # Reset hold repo branches so seed is idempotent across test cases.
    git -C "$HOLD_REPO" branch -D "spira/sp-hold-ahead" 2>/dev/null || true
    git -C "$HOLD_REPO" branch -D "spira/sp-hold-empty" 2>/dev/null || true

    # branch ahead of base: one commit ahead of main
    git -C "$HOLD_REPO" checkout -q -b "spira/sp-hold-ahead"
    git -C "$HOLD_REPO" commit -q --allow-empty -m "sp-hold-ahead: work"
    git -C "$HOLD_REPO" checkout -q main

    # empty branch: branch exists at main HEAD (0 commits ahead)
    git -C "$HOLD_REPO" checkout -q -b "spira/sp-hold-empty"
    git -C "$HOLD_REPO" checkout -q main
}

# ======================================================================================
echo
echo "positive control — a closed bead in a push-mode repo with no commit IS reopened:"
# ======================================================================================
seed
is "sp-push starts closed" closed "$(status_of sp-push)"
out="$(sentinel)"
is "sp-push is reopened" open "$(status_of sp-push)"
want "the pass says so" "reopened sp-push" "$out"

# ======================================================================================
echo
echo "hold, ahead > 0 — closed bead in hold repo with branch commits is NOT reopened:"
# ======================================================================================
seed
is "sp-hold-ahead starts closed" closed "$(status_of sp-hold-ahead)"
out="$(sentinel)"
is "sp-hold-ahead stays closed" closed "$(status_of sp-hold-ahead)"
nowant "the pass does not reopen it" "reopened sp-hold-ahead" "$out"
want "the pass logs land=hold" "land=hold" "$out"

# ======================================================================================
echo
echo "hold, ahead == 0 — closed bead in hold repo with empty branch is NOT reopened:"
# ======================================================================================
seed
is "sp-hold-empty starts closed" closed "$(status_of sp-hold-empty)"
out="$(sentinel)"
is "sp-hold-empty stays closed" closed "$(status_of sp-hold-empty)"
nowant "the pass does not reopen it" "reopened sp-hold-empty" "$out"
want "the pass logs land=hold" "land=hold" "$out"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
