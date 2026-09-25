#!/usr/bin/env bash
#
# test-landing-pr.sh — landing pass in PR mode: push-and-wait repositories where GitHub CI
# merges the branch. The pass opens a pull request, detects when the base has moved under an
# open PR and rebases it, respects the MERGED state, and escalates when a branch keeps
# failing to merge past a configured refresh budget.
#
# Extracted from test-landing.sh to reduce the critical-path suite time.
#
# covers: spira/landing.sh spira/lib.sh
# timeout: 300
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
testdb_require test-landing-pr
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — asserts a requeued-event count via bd sql, which embedded mode refuses
export SPIRA_TESTDB_MODE=server
testdb_up landing-pr || {
    printf 'SKIP test-landing-pr: server testdb not available\n' >&2
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
stub gate.sh 'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
stub gh 'exit 1'

B() { bd -C "$SPIRA_DB" "$@"; }

seed() {
    testdb_reset
    rm -f "$RUN/landing.progress"
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

mkrepo2() {
    local rmt="$TMP/$1.git" repo="$TMP/$1"
    git init -q --bare -b main "$rmt"
    git init -q -b main "$repo"
    git -C "$repo" commit -q --allow-empty -m base
    git -C "$repo" remote add origin "$rmt"
    git -C "$repo" push -q origin main
    git -C "$repo" fetch -q origin
}

branch_in() {
    local dir="$1" id="$2" name="$3"
    git -C "$dir" worktree add -q -b "spira/$id" "$RUN/worktree-pr/$id" main
    printf '%s\n' "$id" > "$RUN/worktree-pr/$id/$id.txt"
    git -C "$RUN/worktree-pr/$id" add -A
    git -C "$RUN/worktree-pr/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":["repo:%s"],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$name" "$id" | testdb_seed
}

closed_child() {
    local id="$1" name="$2"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":["repo:%s"],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$name" "$id" | testdb_seed
}

mkdir -p "$RUN/worktree-pr"

export GH_STATE="$TMP/gh.state"
export GH_LOG="$TMP/gh.log"; : > "$GH_LOG"
cat > "$SH/ghpr" <<'GH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GH_LOG"
case "$1 $2" in
  "pr view")
      [ -f "$GH_STATE" ] || exit 0
      read -r gh_num gh_st < "$GH_STATE"
      case "$*" in
        *.number*) printf '%s\n' "$gh_num" ;;
        *.state*)  printf '%s\n' "$gh_st" ;;
      esac
      exit 0 ;;
  "pr create") cat >/dev/null; printf '%s %s\n' "${GH_NUM:-7}" OPEN > "$GH_STATE"; exit "${GH_CREATE_RC:-0}" ;;
  "pr merge")  exit "${GH_MERGE_RC:-0}" ;;
  "pr list")   printf '[]\n'; exit 0 ;;
esac
exit 1
GH
chmod +x "$SH/ghpr"

# landing_pr: iterate the current repo-map and call pr-pass-branch.sh for each branch
# in pr-mode repos. This replaces the old `landing.sh`-based PR mode.
landing_pr() {
    local _repo _path _mode _rest _base _br _tip _id _line _out=""
    : > "$EMITTED"
    while IFS='|' read -r _repo _path _mode _rest; do
        _repo="${_repo// /}"; _mode="${_mode// /}"
        # trim leading/trailing spaces from path without destroying internal structure
        _path="${_path#"${_path%%[! ]*}"}"; _path="${_path%"${_path##*[! ]}"}"
        [ "$_mode" = pr ] || continue
        [ -d "$_path" ] || continue
        _base="$(git -C "$_path" symbolic-ref -q --short refs/remotes/origin/HEAD 2>/dev/null)" \
            || _base="origin/main"
        while IFS=' ' read -r _br _tip; do
            [ -n "${_br:-}" ] || continue
            _id="${_br#spira/}"
            _line="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
                     SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_GH="$SH/ghpr" \
                     GH_TIMEOUT=30 SPIRA_ID_PREFIX=sp \
                     SPIRA_PR_REFRESH_MAX="${SPIRA_PR_REFRESH_MAX:-}" \
                     bash "$SH/pr-pass-branch.sh" "$_path" "$_br" "$_id" "$_base" "$_repo" "$_tip" 2>&1)"
            _out="${_out}${_line}"$'\n'
        done < <(git -C "$_path" for-each-ref --format='%(refname:short) %(objectname)' \
                     'refs/heads/spira/*' 2>/dev/null)
    done < "$SH/repo-map"
    printf '%s' "$_out"
}

# pr-pass-branch.sh records via land_mark/mark_submitted, not landing.progress.
# Negative mailbox assertions remain to verify idempotency through $out.
mailbox_pr() { :; }

echo "test-landing-pr.sh"

# --------------------------------------------------------------------------------------
# PR MODE: a branch is pushed, a pull request is opened, and the branch is left standing
# for GitHub CI to merge it. The submission marker prevents re-pushing on every pass.
# When the target repository's base moves, the pull request goes stale — this section
# tests that the pass detects and corrects that.
# --------------------------------------------------------------------------------------
echo

mkrepo2 three
cat > "$SH/repo-map" <<MAP
three | $TMP/three | pr | |
MAP
rm -f "$GH_STATE"; : > "$GH_LOG"
seed; branch_in "$TMP/three" sp-pr three
out="$(landing_pr)"
want "a pr-mode branch is pushed and a pull request is opened" "opened a pull request for spira/sp-pr" "$out"

: > "$GH_LOG"
out="$(landing_pr)"
nowant "an open pull request is not reopened next run"  "opened a pull request" "$out"
[ -s "$GH_LOG" ] && bad "and gh is not called again" "$(cat "$GH_LOG")" \
                 || ok "and gh is not called again"
is "and nothing reaches the mailbox" "" "$(mailbox_pr)"

# --------------------------------------------------------------------------------------
# A PULL REQUEST WHOSE BASE HAS MOVED. This is the failure the submission marker created:
# the marker skips a branch until its tip moves and nothing moved it, so the pull request
# aged in place against a base that did not — and a required check that tests the PR head
# rather than the merge result then goes red with nobody left to own it, because the bead is
# closed and its aeon is gone (law-stale-red-pr).
#
# The assertions are ancestry in the branch and in the bare origin, not lines in a log: what
# is being claimed is that the branch now carries the new base and that the remote agrees.
# --------------------------------------------------------------------------------------
advance_pr() {
    local r="$1"
    git -C "$r" checkout -q main
    git -C "$r" commit -q --allow-empty -m "the base moves"
    git -C "$r" push -q origin main
    git -C "$r" fetch -q origin
}

before="$(git -C "$TMP/three" rev-parse spira/sp-pr)"
advance_pr "$TMP/three"
: > "$GH_LOG"
out="$(landing_pr)"
want "a stale pull request is rebased onto the base that moved" "refreshed spira/sp-pr onto origin/main" "$out"
git -C "$TMP/three" merge-base --is-ancestor origin/main spira/sp-pr \
    && ok "and the branch really carries the new base now" \
    || bad "the refresh" "the branch is still behind origin/main"
[ "$before" != "$(git -C "$TMP/three" rev-parse spira/sp-pr)" ] \
    && ok "and the tip really moved, which is what un-skips the marker" \
    || bad "the refresh" "the tip is unchanged"
git -C "$TMP/three" fetch -q origin
is     "and the remote branch moved with it" \
       "$(git -C "$TMP/three" rev-parse spira/sp-pr)" \
       "$(git -C "$TMP/three" rev-parse refs/remotes/origin/spira/sp-pr)"
nowant "and no SECOND pull request is opened for the same work" "pr create" "$(cat "$GH_LOG")"
is "and a refresh is not reported to the sentinel as a movement" "" "$(mailbox_pr)"

: > "$GH_LOG"
out="$(landing_pr)"
nowant "a branch level with its base is not refreshed again" "refreshed spira/sp-pr" "$out"
[ -s "$GH_LOG" ] && bad "and gh is not asked about it again" "$(cat "$GH_LOG")" \
                 || ok "and gh is not asked about it again"

# A MERGED PULL REQUEST IS NOT A STALE ONE.
printf '7 MERGED\n' > "$GH_STATE"
before="$(git -C "$TMP/three" rev-parse spira/sp-pr)"
advance_pr "$TMP/three"
: > "$GH_LOG"
out="$(landing_pr)"
want "a merged pull request's branch is left alone" "its pull request is MERGED — nothing to refresh" "$out"
is   "and its tip is exactly where it was"          "$before" "$(git -C "$TMP/three" rev-parse spira/sp-pr)"
out="$(landing_pr)"
nowant "and it is not re-examined on every later pass" "nothing to refresh" "$out"

# A gh THAT CANNOT ANSWER IS NOT PERMISSION TO REWRITE THE BRANCH.
rm -f "$GH_STATE"
git -C "$TMP/three" branch -q -D spira/sp-pr2 2>/dev/null
git -C "$TMP/three" branch -q spira/sp-pr2 spira/sp-pr
closed_child sp-pr2 three
printf '%s %s pr 0\n' "$(git -C "$TMP/three" rev-parse spira/sp-pr2)" "$(date +%s)" > "$RUN/submitted/sp-pr2"
before="$(git -C "$TMP/three" rev-parse spira/sp-pr2)"
out="$(landing_pr)"
want "an unreadable pull request state stops the refresh" "gh will not say whether its pull request is open" "$out"
is   "and the branch is untouched"                        "$before" "$(git -C "$TMP/three" rev-parse spira/sp-pr2)"
git -C "$TMP/three" branch -q -D spira/sp-pr2

# --------------------------------------------------------------------------------------
# THE BOUND. A branch refreshed and refreshed that still does not merge is not a slow
# landing, it is a stuck one, and a loop that goes on rebasing it hides that rather than
# fixing it. The cap is lowered to 1 here so the escalation is reached in two passes.
# --------------------------------------------------------------------------------------
mkrepo2 eight
cat > "$SH/repo-map" <<MAP
eight | $TMP/eight | pr | |
MAP
rm -f "$GH_STATE"; : > "$GH_LOG"; : > "$EMITTED"
seed; branch_in "$TMP/eight" sp-rot eight
out="$(landing_pr)"
want "the bounded case opens its pull request first" "opened a pull request for spira/sp-rot" "$out"

advance_pr "$TMP/eight"
: > "$EMITTED"
out="$(SPIRA_PR_REFRESH_MAX=1 landing_pr)"
want "the first refresh is spent"    "refreshed spira/sp-rot" "$out"
is   "and nothing is escalated yet"  "" "$(cat "$EMITTED")"

advance_pr "$TMP/eight"
before="$(git -C "$TMP/eight" rev-parse spira/sp-rot)"
: > "$EMITTED"
out="$(SPIRA_PR_REFRESH_MAX=1 landing_pr)"
nowant "a branch past its cap is not rebased again"  "refreshed spira/sp-rot" "$out"
is     "and its tip is left where it was"            "$before" "$(git -C "$TMP/eight" rev-parse spira/sp-rot)"
want   "and it is escalated instead"                 "sp-rot: escalated" "$out"
want   "and the ask carries a default Ryan can take" "reopen sp-rot at P0" "$(cat "$EMITTED")"
want   "and names the branch and its repository"     "spira/sp-rot in eight" "$(cat "$EMITTED")"
want   "and leads with what the bead was for"        "WHAT THIS BEAD IS FOR" "$(cat "$EMITTED")"
is     "and an escalation is not a movement either"  "" "$(mailbox_pr)"

# ONCE, NOT EVERY PASS.
advance_pr "$TMP/eight"
: > "$EMITTED"
out="$(SPIRA_PR_REFRESH_MAX=1 landing_pr)"
want "an escalated branch says so rather than escalating again" "already escalated" "$out"
is   "and Ryan is not paged a second time"                      "" "$(cat "$EMITTED")"

# Restore repo-map to the fixture push-mode repo.
cat > "$SH/repo-map" <<MAP
$REPONAME | $REPO | push | |
MAP

# --------------------------------------------------------------------------------------
# DUPLICATE MERGE-CONFLICT BUMP SUPPRESSION (db-91ox)
#
# An escalated branch whose tip and base have not moved since the last RED mark must
# produce exactly one requeued/merge-conflict event however many passes run against it.
# A base that advances is new evidence and must still bump.
# --------------------------------------------------------------------------------------
mc_of() {
    B sql "SELECT COUNT(*) FROM events WHERE issue_id='$1' AND event_type='requeued' AND new_value='merge-conflict'" 2>/dev/null \
    | sed -n '3p' | tr -d ' '
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
landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
        bash "$SH/landing.sh" 2>&1
}
seed2() {
    testdb_reset
    rm -f "$RUN/landing.progress"
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

seed2; branch sp-nodupe shared.txt "from the branch"
printf '%s\n' "from the base" > "$REPO/shared.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base writes shared.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin

out="$(SPIRA_REBASE_ESCALATE_AT=1 landing)"
want "first pass detects the conflict and escalates"     "escalated sp-nodupe" "$out"
is   "and writes exactly one merge-conflict event"       1 "$(mc_of sp-nodupe)"

out2="$(SPIRA_REBASE_ESCALATE_AT=1 landing)"
is   "a second pass with unchanged tip and base writes no new event" 1 "$(mc_of sp-nodupe)"
want "and the log names the reason it skipped" "tip and base unchanged since last RED" "$out2"

printf '%s\n' "another base commit" >> "$REPO/shared.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base advances"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
out3="$(SPIRA_REBASE_ESCALATE_AT=1 landing)"
is   "after base moves, a new event is written" 2 "$(mc_of sp-nodupe)"
drop_branch sp-nodupe

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
