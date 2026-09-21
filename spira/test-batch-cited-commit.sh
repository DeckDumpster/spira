#!/usr/bin/env bash
#
# test-batch-cited-commit.sh — bead_cited_commit_on_base requires an explicit
#   hand-landed citation, never a bare SHA in prose.
#
# THE DEFECT (sp-c9d41). bead_cited_commit_on_base accepted any 7-40 hex string
# in a bead's notes as evidence the fix had landed, causing batch.sh to mark beads
# LANDED and retire their branches when a commit SHA appeared in prose context.
#
# THE FIX. bead_cited_commit_on_base now accepts a sha only when:
#   - the note uses "landed as <sha>" or "hand-landed <sha>", or
#   - the commit message at that sha names the bead id.
# A bare sha in prose is never sufficient. The cited-on-main path no longer
# destroys the bead's branch.
#
# SIX UNIT CASES for bead_cited_commit_on_base (law-absence-needs-a-positive-control):
#
#   1. BARE-SHA — note cites HAND_SHA in prose ("re-applied on <sha>"); commit
#      message does not name the bead → no match.  Prevents the incident that
#      triggered this bead: bare shas in context notes must not trigger landing.
#
#   2. DECLARED — note "landed as <sha>" where sha is on base → returns sha and
#      "cited-declared".  Positive control: the declared path can fire.
#
#   3. DECLARED-VARIANT — note "hand-landed <sha>" → returns sha and "cited-declared".
#      Confirms the second explicit phrase is accepted.
#
#   4. NAMED — bare sha in note where the commit message names the bead id
#      → returns sha and "cited-named".  Positive control: the named path can fire.
#
#   5. WRONG-HEX — note contains a hex string that is not a real commit → no match.
#
#   6. SHA-NOT-ON-BASE — note "landed as <sha>" where sha is NOT an ancestor of
#      base → no match.  Ancestry check applies to explicit declarations too.
#
# ONE INTEGRATION CASE for batch.sh (positive control for the reopen path):
#
#   7. INTEGRATION-CTRL — conflicting branch with no cited commit, no declared
#      phrase → batch reopens the bead and preserves the branch.  Proves the
#      conflict-detector fires and the reopen path is not vacuously silent.
#
# defect: sp-c9d41
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

# Initial commit.
printf 'original\n' > "$REPO/f.txt"
git -C "$REPO" add f.txt && git -C "$REPO" commit -q -m "initial"
INITIAL_SHA="$(git -C "$REPO" rev-parse HEAD)"

# HAND_SHA: the hand-applied fix on main. Commit message deliberately does not
# name any test bead id so only the declared path can match it.
printf 'handfix\n' > "$REPO/f.txt"
git -C "$REPO" add f.txt && git -C "$REPO" commit -q -m "handfix applied"
HAND_SHA="$(git -C "$REPO" rev-parse HEAD)"

# NAMED_SHA: commit whose message names "sp-named", for the named-path test.
printf 'named-extra\n' > "$REPO/g.txt"
git -C "$REPO" add g.txt && git -C "$REPO" commit -q -m "sp-named: applied here"
NAMED_SHA="$(git -C "$REPO" rev-parse HEAD)"

git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main

BASE="$NAMED_SHA"

# SIDE_SHA: a commit NOT on main, for the sha-not-on-base test.
git -C "$REPO" branch side "$INITIAL_SHA"
git -C "$REPO" worktree add -q "$TMP/side" side
printf 'side\n' > "$TMP/side/h.txt"
git -C "$TMP/side" add h.txt && git -C "$TMP/side" commit -q -m "side commit"
SIDE_SHA="$(git -C "$REPO" rev-parse side)"
git -C "$REPO" worktree remove "$TMP/side" 2>/dev/null || true

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

# mail.sh stub.
printf '#!/usr/bin/env bash\ntrue\n' > "$SH/mail.sh"; chmod +x "$SH/mail.sh"

# Repo-map: queue mode.
printf '%s | %s | queue | origin/main | | |\n' "$REPONAME" "$REPO" > "$SH/repo-map"

B() { bd -C "$SPIRA_DB" "$@"; }

# cited_on_base <id> — call bead_cited_commit_on_base against the test repo and base.
cited_on_base() {
    local _id="$1"
    (
        SPIRA_HOME="$SH"
        SPIRA_RUN="$RUN"
        SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}"
        export SPIRA_HOME SPIRA_RUN SPIRA_BD
        # shellcheck disable=SC1090
        . "$SH/lib.sh"
        bead_cited_commit_on_base "$_id" "$REPO" "$BASE"
    ) 2>/dev/null
}

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

status_of()   { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status",""))' 2>/dev/null; }

branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/spira/$1" 2>/dev/null; }

# Seed all beads for unit tests (notes are set once, not reset between cases).
testdb_reset || { echo "seed: testdb_reset failed" >&2; exit 1; }
testdb_seed <<JSONL || { echo "seed: testdb_seed failed" >&2; exit 1; }
{"id":"sp-bare","title":"bare sha test","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-21T00:00:00Z"}
{"id":"sp-decl","title":"declared sha test","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-21T00:00:00Z"}
{"id":"sp-decl2","title":"hand-landed sha test","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-21T00:00:00Z"}
{"id":"sp-named","title":"named commit test","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-21T00:00:00Z"}
{"id":"sp-wrong","title":"wrong hex test","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-21T00:00:00Z"}
{"id":"sp-nobase","title":"sha not on base test","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-21T00:00:00Z"}
{"id":"sp-ctrl","title":"positive control — no cited commit","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-21T00:00:00Z"}
JSONL
B note sp-bare  "re-applied on $HAND_SHA when rebasing locally"                     >/dev/null 2>&1 || true
B note sp-decl  "landed as $HAND_SHA"                                                >/dev/null 2>&1 || true
B note sp-decl2 "hand-landed $HAND_SHA in production"                               >/dev/null 2>&1 || true
B note sp-named "patch originally at $NAMED_SHA, see review"                        >/dev/null 2>&1 || true
B note sp-wrong "related to tracker ticket deadbeef1234567, not a commit"            >/dev/null 2>&1 || true
B note sp-nobase "landed as $SIDE_SHA (side branch, not on main)"                   >/dev/null 2>&1 || true

echo "test-batch-cited-commit.sh"

# =============================================================================
# FIXTURE SANITY: verify the commits are in the expected positions.
# =============================================================================
echo
echo "fixture sanity:"
if git -C "$REPO" merge-base --is-ancestor "$HAND_SHA" "$BASE" 2>/dev/null; then
    ok "HAND_SHA is an ancestor of BASE"
else
    bad "HAND_SHA is an ancestor of BASE" "is-ancestor returned false"
fi
if git -C "$REPO" merge-base --is-ancestor "$NAMED_SHA" "$BASE" 2>/dev/null; then
    ok "NAMED_SHA is an ancestor of BASE"
else
    bad "NAMED_SHA is an ancestor of BASE" "is-ancestor returned false"
fi
if ! git -C "$REPO" merge-base --is-ancestor "$SIDE_SHA" "$BASE" 2>/dev/null; then
    ok "SIDE_SHA is NOT an ancestor of BASE"
else
    bad "SIDE_SHA is NOT an ancestor of BASE" "is-ancestor returned true"
fi

# =============================================================================
# UNIT CASES 1-6: bead_cited_commit_on_base
# =============================================================================

# CASE 1: BARE-SHA — bare sha in prose does not trigger the cited path.
echo
echo "case 1 — bare sha in prose: no match:"
out="$(cited_on_base sp-bare)"
is  "bare sha: no output"        ""    "$out"

# CASE 2: DECLARED — "landed as <sha>" → sha cited-declared.
echo
echo "case 2 — declared (landed as): match with cited-declared:"
out="$(cited_on_base sp-decl)"
want "declared: sha present"     "${HAND_SHA:0:7}"   "$out"
want "declared: rule is cited-declared" "cited-declared" "$out"

# CASE 3: DECLARED-VARIANT — "hand-landed <sha>" → sha cited-declared.
echo
echo "case 3 — declared (hand-landed): match with cited-declared:"
out="$(cited_on_base sp-decl2)"
want "hand-landed: sha present"  "${HAND_SHA:0:7}"   "$out"
want "hand-landed: rule is cited-declared" "cited-declared" "$out"

# CASE 4: NAMED — bare sha where the commit message names the bead id.
echo
echo "case 4 — named commit (commit message names bead): match with cited-named:"
out="$(cited_on_base sp-named)"
want "named: sha present"        "${NAMED_SHA:0:7}"  "$out"
want "named: rule is cited-named" "cited-named"       "$out"

# CASE 5: WRONG-HEX — hex string that is not a real commit: no match.
echo
echo "case 5 — hex string not a real commit: no match:"
out="$(cited_on_base sp-wrong)"
is  "wrong-hex: no output"       ""    "$out"

# CASE 6: SHA-NOT-ON-BASE — declared phrase but sha is not an ancestor of base.
echo
echo "case 6 — declared sha not on base: no match:"
out="$(cited_on_base sp-nobase)"
is  "not-on-base: no output"     ""    "$out"

# =============================================================================
# INTEGRATION CASE 7: batch.sh reopens a conflicting branch with no cited SHA.
# This exercises the batch.sh flow to prove the reopen path is still intact.
# =============================================================================
echo
echo "case 7 — integration: conflicting branch, no valid citation → reopened:"

# Create a conflicting branch for sp-ctrl.
_ctrl_init="$(git -C "$REPO" rev-parse origin/main~2)"  # before HAND_SHA
git -C "$REPO" branch "spira/sp-ctrl" "$_ctrl_init"
git -C "$REPO" worktree add -q "$RUN/worktree/sp-ctrl" "spira/sp-ctrl"
printf 'aeon-version\n' > "$RUN/worktree/sp-ctrl/f.txt"
git -C "$RUN/worktree/sp-ctrl" add f.txt
git -C "$RUN/worktree/sp-ctrl" commit -q -m "sp-ctrl: fix via aeon"
git -C "$REPO" worktree remove "$RUN/worktree/sp-ctrl" 2>/dev/null || true

_ctrl_tip="$(git -C "$REPO" rev-parse spira/sp-ctrl)"
printf 'CERTIFIED %s %s\n' "$_ctrl_tip" "$(date +%s)" > "$LANDSTATE/sp-ctrl"

out="$(batch "$REPONAME")"
want  "batch reports conflict"     "conflicts with"  "$out"
want  "batch reports reopen"       "reopened"        "$out"
nowant "batch did not cite"        "notes cite"      "$out"
is    "sp-ctrl is reopened"        open              "$(status_of sp-ctrl)"
if branch_exists sp-ctrl; then ok "sp-ctrl branch survives reopen"; \
else bad "sp-ctrl branch survives reopen" "branch was deleted"; fi

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
