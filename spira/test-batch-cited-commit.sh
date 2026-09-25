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
# A bare sha in prose is never sufficient.
#
# SIX UNIT CASES (law-absence-needs-a-positive-control), pure function against
# a git repo and a canned bead — no batch.sh, no bd. bead_cited_commit_on_base's
# only bd read is one `bdjson show <id>` for the notes field, so a static
# SPIRA_BDJSON_FIXTURE answers it (lib.sh:bdq) without a database (sp-s088v.16,
# demoted to T2: what batch.sh actually DOES with the answer — reopen vs mark
# landed — is the integration table in test-batch-conflict.sh, cases 7-9, which
# needs a real bd to read the reopen back and stays at the default tier).
#
#   1. BARE-SHA — note cites HAND_SHA in prose ("re-applied on <sha>"); commit
#      message does not name the bead → no match.
#   2. DECLARED — note "landed as <sha>" where sha is on base → cited-declared.
#   3. DECLARED-VARIANT — note "hand-landed <sha>" → cited-declared.
#   4. NAMED — bare sha where the commit message names the bead id → cited-named.
#   5. WRONG-HEX — a hex string that is not a real commit → no match.
#   6. SHA-NOT-ON-BASE — "landed as <sha>" where sha is not an ancestor of base
#      → no match; the ancestry check applies to explicit declarations too.
#
# defect: sp-c9d41
# covers: spira/lib.sh
# tier: T2
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
git init -q -b main "$REPO"

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
BASE="$NAMED_SHA"

# SIDE_SHA: a commit NOT on main, for the sha-not-on-base test.
git -C "$REPO" branch side "$INITIAL_SHA"
git -C "$REPO" worktree add -q "$TMP/side" side
printf 'side\n' > "$TMP/side/h.txt"
git -C "$TMP/side" add h.txt && git -C "$TMP/side" commit -q -m "side commit"
SIDE_SHA="$(git -C "$REPO" rev-parse side)"
git -C "$REPO" worktree remove "$TMP/side" 2>/dev/null || true

mkdir -p "$TMP/spira-run"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/bdsim.py" "$TMP/"

# Stub notes, no bd: one static fixture array, keyed by id. bdjson show reads
# the "notes" field straight from here (lib.sh:bdq / SPIRA_BDJSON_FIXTURE).
BD_FIXTURE="$TMP/bd.json"
cat > "$BD_FIXTURE" <<JSON
[
  {"id":"sp-bare",  "notes": ["re-applied on $HAND_SHA when rebasing locally"]},
  {"id":"sp-decl",  "notes": ["landed as $HAND_SHA"]},
  {"id":"sp-decl2", "notes": ["hand-landed $HAND_SHA in production"]},
  {"id":"sp-named", "notes": ["patch originally at $NAMED_SHA, see review"]},
  {"id":"sp-wrong", "notes": ["related to tracker ticket deadbeef1234567, not a commit"]},
  {"id":"sp-nobase","notes": ["landed as $SIDE_SHA (side branch, not on main)"]}
]
JSON

# cited_on_base <id> — call bead_cited_commit_on_base against the test repo and base.
cited_on_base() {
    local _id="$1"
    (
        SPIRA_RUN="$TMP/spira-run"
        SPIRA_BDJSON_FIXTURE="$BD_FIXTURE"
        export SPIRA_RUN SPIRA_BDJSON_FIXTURE
        # shellcheck disable=SC1090
        . "$TMP/lib.sh"
        bead_cited_commit_on_base "$_id" "$REPO" "$BASE"
    ) 2>/dev/null
}

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

echo
echo "case 1 — bare sha in prose: no match:"
out="$(cited_on_base sp-bare)"
is  "bare sha: no output"        ""    "$out"

echo
echo "case 2 — declared (landed as): match with cited-declared:"
out="$(cited_on_base sp-decl)"
want "declared: sha present"     "${HAND_SHA:0:7}"   "$out"
want "declared: rule is cited-declared" "cited-declared" "$out"

echo
echo "case 3 — declared (hand-landed): match with cited-declared:"
out="$(cited_on_base sp-decl2)"
want "hand-landed: sha present"  "${HAND_SHA:0:7}"   "$out"
want "hand-landed: rule is cited-declared" "cited-declared" "$out"

echo
echo "case 4 — named commit (commit message names bead): match with cited-named:"
out="$(cited_on_base sp-named)"
want "named: sha present"        "${NAMED_SHA:0:7}"  "$out"
want "named: rule is cited-named" "cited-named"       "$out"

echo
echo "case 5 — hex string not a real commit: no match:"
out="$(cited_on_base sp-wrong)"
is  "wrong-hex: no output"       ""    "$out"

echo
echo "case 6 — declared sha not on base: no match:"
out="$(cited_on_base sp-nobase)"
is  "not-on-base: no output"     ""    "$out"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
