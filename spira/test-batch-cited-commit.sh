#!/usr/bin/env bash
#
# test-batch-cited-commit.sh — batch.sh marks a CERTIFIED branch landed (not reopened)
#   when the bead's notes cite a commit already on the base.
#
# THE DEFECT (sp-ohu7i). When a fix is hand-landed from a different branch (e.g. concierge
# session) and the bead's own spira/<id> branch carries a conflicting version, batch.sh
# reopened the bead ("conflicts with origin/main"). The same fix already on main makes the
# rebase impossible: the branch cycles through CERTIFIED → conflicts → reopened forever.
#
# THE FIX. Before reopening a branch that conflicts with the base, batch.sh checks the
# bead's notes for a commit SHA that is an ancestor of the base. If it finds one, the fix
# has already landed: batch.sh marks the bead LANDED and retires the branch instead.
#
# THREE CASES (law-absence-needs-a-positive-control):
#
#   1. POSITIVE CONTROL — a conflicting branch with NO note citing a commit on main IS
#      reopened. Proves the conflict detector fires and the check is not vacuously silent.
#
#   2. CITED — a conflicting branch whose bead's notes cite a commit on main is marked
#      LANDED and its branch retired, never reopened. This is the arm SEEN TO FAIL before
#      the fix: the bead is reopened on unfixed batch.sh.
#
#   3. CITED-WRONG — a note citing a hex-looking string that is NOT a commit on main does
#      not trigger the landed path; the bead is reopened normally.
#
# defect: sp-ohu7i
# covers: spira/batch.sh spira/lib.sh
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
testdb_require test-batch-cited-commit
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchcited || { echo "test-batch-cited-commit: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" remote add origin "$REMOTE"

# Initial commit: a file that multiple branches will all touch (creates the conflict).
printf 'original\n' > "$REPO/shared.txt"
git -C "$REPO" add shared.txt && git -C "$REPO" commit -q -m "initial"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main

# Hand-fix commit: represents the fix that landed from another branch. This is what
# spira/<id> branches will conflict with, and what the bead's note will cite.
printf 'hand-fixed\n' > "$REPO/shared.txt"
git -C "$REPO" add shared.txt && git -C "$REPO" commit -q -m "fix: hand-landed from concierge"
HAND_SHA="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"
cp "$HERE"/*.sh "$SH/"

# Gate stub — always pass.
cat > "$SH/gate.sh" <<'GSTUB'
#!/usr/bin/env bash
exit 0
GSTUB
chmod +x "$SH/gate.sh"

# Forge stub — records pr-create calls.
FORGE_LOG="$TMP/forge-log"
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    pr-create)
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n" >> "$FORGE_LOG"
        printf '%s\n' "$n" ;;
    pr-number)
        grep "^${1:-}	" "$FORGE_LOG" 2>/dev/null | tail -1 | cut -f2 ;;
    *) printf 'forge: unknown: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
: > "$FORGE_LOG"

# mail.sh stub — swallow mail, this suite does not assert on mail.
printf '#!/usr/bin/env bash\ntrue\n' > "$SH/mail.sh"; chmod +x "$SH/mail.sh"

# Repo-map: queue mode.
printf '%s | %s | queue | origin/main | | |\n' "$REPONAME" "$REPO" > "$SH/repo-map"

B() { bd -C "$SPIRA_DB" "$@"; }

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=1 \
    SPIRA_QUEUE_BATCH_WAIT=0 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status",""))' 2>/dev/null; }

branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/spira/$1" 2>/dev/null; }

landstate_of() { awk '{print $1}' "$LANDSTATE/$1" 2>/dev/null; }

# make_conflicting_branch <id> — cut a branch from the initial commit and modify shared.txt
# in a way that conflicts with the hand-fix commit on main.
make_conflicting_branch() {
    local id="$1" initial
    initial="$(git -C "$REPO" rev-parse origin/main~1)"  # commit before hand-fix
    git -C "$REPO" branch "spira/$id" "$initial"
    # Add a worktree, write a conflicting value, commit.
    git -C "$REPO" worktree add -q "$RUN/worktree/$id" "spira/$id"
    printf 'aeon-version\n' > "$RUN/worktree/$id/shared.txt"
    git -C "$RUN/worktree/$id" add shared.txt
    git -C "$RUN/worktree/$id" commit -q -m "$id: fix via aeon"
    git -C "$REPO" worktree remove "$RUN/worktree/$id" 2>/dev/null || true
}

seed() {
    testdb_reset || { echo "seed: testdb_reset failed" >&2; exit 1; }
    testdb_seed <<JSONL || { echo "seed: testdb_seed failed" >&2; exit 1; }
{"id":"sp-ctrl","title":"positive control — no cited commit","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-19T00:00:00Z"}
{"id":"sp-cited","title":"bead with cited commit in notes","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-19T00:00:00Z"}
{"id":"sp-wrong","title":"bead with non-commit hex in notes","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-19T00:00:00Z"}
JSONL
    # Add a note to sp-cited that cites the hand-fix SHA.
    B note sp-cited "Fix already landed from concierge as commit $HAND_SHA (PR #74)." >/dev/null 2>&1 || true
    # Add a note to sp-wrong with a plausible but non-commit hex string.
    B note sp-wrong "Related to ticket deadbeef1234567 in external tracker." >/dev/null 2>&1 || true
}

echo "test-batch-cited-commit.sh"

# =============================================================================
# SETUP: build conflicting branches for all three beads.
# =============================================================================
make_conflicting_branch sp-ctrl
make_conflicting_branch sp-cited
make_conflicting_branch sp-wrong

# Verify the branches actually conflict with origin/main before asserting batch behaviour.
echo
echo "fixture sanity — all branches conflict with origin/main:"
for _id in sp-ctrl sp-cited sp-wrong; do
    _init="$TMP/ck-$_id"
    if git -C "$REPO" worktree add -q --detach "$_init" "$(git -C "$REPO" rev-parse origin/main)" 2>/dev/null; then
        if git -C "$_init" merge --no-commit --no-ff "spira/$_id" >/dev/null 2>&1; then
            bad "spira/$_id should conflict with origin/main" "merge succeeded"
        else
            ok "spira/$_id conflicts with origin/main (fixture correct)"
        fi
        git -C "$_init" merge --abort 2>/dev/null || true
        git -C "$REPO" worktree remove -f "$_init" 2>/dev/null || true
    else
        bad "spira/$_id conflict check" "worktree add failed"
    fi
done

# =============================================================================
# CASE 1: POSITIVE CONTROL — no cited commit → normal reopen.
# =============================================================================
echo
echo "positive control — conflicting branch with no cited commit IS reopened:"

seed
_ctrl_tip="$(git -C "$REPO" rev-parse spira/sp-ctrl)"
printf 'CERTIFIED %s %s\n' "$_ctrl_tip" "$(date +%s)" > "$LANDSTATE/sp-ctrl"

out="$(batch "$REPONAME")"
want "batch logs the conflict"       "conflicts with"         "$out"
want "batch reports reopened"        "reopened"               "$out"
is   "sp-ctrl is reopened"          open                     "$(status_of sp-ctrl)"
# Branch should still exist (not retired) after reopen.
if branch_exists sp-ctrl; then ok "sp-ctrl branch still exists after reopen"; \
else bad "sp-ctrl branch still exists after reopen" "branch was deleted"; fi

# =============================================================================
# CASE 2: CITED — note cites a commit on main → marked LANDED, branch retired.
# This is the arm SEEN TO FAIL on unfixed batch.sh: the bead is reopened instead.
# =============================================================================
echo
echo "cited commit on main — LANDED and branch retired, not reopened:"

seed
_cited_tip="$(git -C "$REPO" rev-parse spira/sp-cited)"
printf 'CERTIFIED %s %s\n' "$_cited_tip" "$(date +%s)" > "$LANDSTATE/sp-cited"

out="$(batch "$REPONAME")"
want "batch reports cited-commit path" "notes cite"            "$out"
want "batch reports landed"            "marked landed"         "$out"
nowant "batch does not reopen it"      "reopened"              "$out"
is   "sp-cited stays closed"          closed                  "$(status_of sp-cited)"
is   "sp-cited landstate is LANDED"   LANDED                  "$(landstate_of sp-cited)"
if ! branch_exists sp-cited; then ok "sp-cited branch is gone (retired)"; \
else bad "sp-cited branch is gone (retired)" "branch still exists"; fi

# =============================================================================
# CASE 3: CITED-WRONG — note cites hex that is not a commit → normal reopen.
# =============================================================================
echo
echo "hex-looking note that is not a commit on main → normal reopen:"

seed
_wrong_tip="$(git -C "$REPO" rev-parse spira/sp-wrong)"
printf 'CERTIFIED %s %s\n' "$_wrong_tip" "$(date +%s)" > "$LANDSTATE/sp-wrong"

out="$(batch "$REPONAME")"
nowant "batch does not report cited-commit path" "notes cite"  "$out"
is    "sp-wrong is reopened"                    open           "$(status_of sp-wrong)"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
