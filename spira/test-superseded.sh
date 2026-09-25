#!/usr/bin/env bash
#
# test-superseded.sh — landing.sh leaves superseded beads alone; sending.sh reaps them.
#
#   ./test-superseded.sh
#
# THE CASE THIS IS WRITTEN FOR (sp-amac). A bead retired with `bd supersede` still has its
# branch in refs/heads/spira/*. That branch is a duplicate by definition — its work landed
# under the successor's id — so it does not rebase onto the base, and the old landing pass
# reopened the retired bead saying it "does not rebase". Discovered on sp-dvlq, which was
# superseded by sp-35pl.
#
# TWO HALVES. The landing fix (skip rather than reopen) is the guard that prevents the false
# alarm. The sending fix (reap) is what makes the branch disappear so the skip is rarely
# needed. Both must hold, and the positive control for each is an identical bead that is NOT
# superseded — because every silence below would pass just as well against a version of the
# fix that skipped every branch, or reaped nothing.
#
# defect: sp-amac
# covers: spira/landing.sh spira/sending.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-superseded
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up superseded || { echo "test-superseded: could not build a fixture database"; exit 1; }
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

cp "$HERE/landing.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/incident.sh" \
   "$HERE/skew.sh" "$HERE/sending.sh" "$HERE/suite-covers.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub gate.sh 'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
stub gh 'exit 1'

B() { bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
        bash "$SH/landing.sh" 2>&1
}

sending() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" \
        bash "$SH/sending.sh" 2>&1
}

seed() {
    testdb_reset
    rm -rf "$RUN/tip-at-gate"
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-succ","title":"successor","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-succ","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
}

branch() {               # branch <id> [file] [content] — a closed bead with a branch of its own
    local id="$1" f="${2:-$id.txt}" c="${3:-$id}"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$c" > "$RUN/worktree/$id/$f"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}

superseded_branch() {    # superseded_branch <id> [file] [content] — like branch, but with supersedes dependency
    local id="$1" f="${2:-$id.txt}" c="${3:-$id}"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$c" > "$RUN/worktree/$id/$f"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"},{"issue_id":"%s","depends_on_id":"sp-succ","type":"supersedes"}]}\n' \
        "$id" "$id" "$id" "$id" | testdb_seed
}

drop_branch() {
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1
}

advance_base() {         # advance_base <file> <content> — push a commit to origin/main
    local f="$1" c="$2"
    printf '%s\n' "$c" > "$REPO/$f"
    git -C "$REPO" add -A
    git -C "$REPO" commit -q -m "base: $f"
    git -C "$REPO" push -q origin main
    git -C "$REPO" fetch -q origin
}

echo "test-superseded.sh"

# --------------------------------------------------------------------------------------
# SETUP: advance the base so superseded's branch conflicts but plain's does not.
#
# Both branches write conflict.txt; only the superseded one should be on the conflict path.
# The plain branch writes plain.txt and rebases cleanly.
# --------------------------------------------------------------------------------------
seed
# Create the superseded branch first (writes conflict.txt).
superseded_branch sp-sup conflict.txt "from the superseded branch"
# Create the plain branch (writes a different file — rebases cleanly).
branch sp-pln plain.txt "from the plain branch"
# Advance the base: write conflict.txt so sp-sup's rebase would conflict.
advance_base conflict.txt "from the base (landed by successor)"

# --------------------------------------------------------------------------------------
# THE LANDING PASS SKIPS SUPERSEDED BEADS.
#
# sp-pln IS rebased and lands (the positive control — proves landing is running and the
# exemption is not just silencing everything).
# sp-sup is skipped: no reopen, no "conflicts in" message.
# --------------------------------------------------------------------------------------
out="$(landing)"
echo "$out" | grep -E "CHECK6|landed|reopened|superseded" | head -20 >&2

want "the plain branch lands (positive control: landing runs)"    "landed spira/sp-pln" "$out"
nowant "the superseded branch is not reopened"                    "reopened sp-sup" "$out"
nowant "and no conflict is reported for it"                       "conflicts in" "$(printf '%s' "$out" | grep "sp-sup")"
want "and the pass names the reason it was skipped"               "superseded" "$(printf '%s' "$out" | grep "sp-sup")"
is   "the plain bead stays closed"                                closed "$(status_of sp-pln)"
is   "the superseded bead stays closed"                           closed "$(status_of sp-sup)"
drop_branch sp-pln

# --------------------------------------------------------------------------------------
# THE SENDING REAPS THE SUPERSEDED BRANCH.
#
# sp-sup's branch is still there after landing (landing skipped it; nothing landed it).
# Sending should reap it because it is superseded, even though content_landed is false.
# --------------------------------------------------------------------------------------
git -C "$REPO" fetch -q origin
out="$(sending)"
echo "$out" | head -20 >&2

want "the Sending reaps the superseded branch"                    "REAPED sp-sup" "$out"
nowant "it does not keep it as unlanded"                          "KEEP   sp-sup" "$out"
# Verify the branch is actually gone.
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-sup" 2>/dev/null; then
    bad "the branch is actually gone" "spira/sp-sup still exists after reap"
else
    ok "the branch is actually gone"
fi

# --------------------------------------------------------------------------------------
# A SUPERSEDED BRANCH THAT ADDS UNIQUE CONTENT IS KEPT, NOT REAPED. (sp-bxd0)
#
# The supersedes edge is a claim, not proof. If the superseded branch would merge without
# conflict — meaning it adds content not on the base — the Sending must not reap it.
# Reaping would silently destroy work the successor never carried.
#
# This is the shape of the near-miss on 2026-09-08 (sp-r6wf): a branch marked superseded
# on the strength of matching titles, but carrying a 115-line test file that existed
# nowhere else.
#
# The positive control is the previous case (REAPED sp-sup): a truly superseded branch
# DOES get reaped. The distinction: sp-sup conflicts with the base (same file changed by
# both successor and superseded branch), while sp-unique adds a new file that creates no
# conflict.
# --------------------------------------------------------------------------------------
seed
git -C "$REPO" worktree add -q -b "spira/sp-unique" "$RUN/worktree/sp-unique" main
printf 'unique content never seen elsewhere\n' > "$RUN/worktree/sp-unique/unique-file.txt"
git -C "$RUN/worktree/sp-unique" add -A
git -C "$RUN/worktree/sp-unique" commit -q -m "feat: sp-unique — add file not in base"
# Mark it superseded (same structure as superseded_branch, but the base is NOT advanced
# with conflicting content — so merge-tree will exit 0 for this branch).
printf '{"id":"sp-unique","title":"sp-unique","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-unique","depends_on_id":"sp-goal","type":"parent-child"},{"issue_id":"sp-unique","depends_on_id":"sp-succ","type":"supersedes"}]}\n' \
    | testdb_seed
git -C "$REPO" fetch -q origin
out="$(sending)"
echo "$out" | head -20 >&2

want "superseded branch with unique content is kept"  "KEEP   sp-unique" "$out"
nowant "it is not reaped"                              "REAPED sp-unique" "$out"
want "and the reason names the unlanded commits"       "unlanded" "$out"
# Branch must still exist — its unique file would be lost if reaped.
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-unique" 2>/dev/null; then
    ok "branch with unique content survives a sending pass"
else
    bad "branch with unique content survives a sending pass" "branch was reaped — unique-file.txt would be lost"
fi
# The branch is kept, but nothing needs to hold its worktree open any longer — a
# standing worktree is what made spira_destroy_branch refuse a later, correctly
# superseded branch (sp-bf31a).
if [ -e "$RUN/worktree/sp-unique" ]; then
    bad "kept branch's worktree is freed" "worktree still present after sending"
else
    ok "kept branch's worktree is freed (branch itself untouched)"
fi
git -C "$REPO" worktree remove --force "$RUN/worktree/sp-unique" >/dev/null 2>&1 || true
git -C "$REPO" branch -D "spira/sp-unique" >/dev/null 2>&1 || true

# --------------------------------------------------------------------------------------
# THE POSITIVE CONTROL FOR SENDING: a non-superseded unlanded branch is KEPT, not reaped.
# --------------------------------------------------------------------------------------
seed; branch sp-kept kept.txt "kept branch"
# Don't advance the base — sp-kept is unlanded and not superseded.
out="$(sending)"
want "a non-superseded unlanded branch is kept"   "KEEP   sp-kept" "$out"
nowant "and is not reaped"                         "REAPED sp-kept" "$out"
drop_branch sp-kept

# --------------------------------------------------------------------------------------
# DRY-RUN NAMES THE SUPERSEDED BRANCH CORRECTLY.
# --------------------------------------------------------------------------------------
seed
superseded_branch sp-drysup conflict.txt "dry run test"
advance_base conflict.txt "base content for dry run"
git -C "$REPO" fetch -q origin
out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$SH/repo-map" \
    bash "$SH/sending.sh" --dry-run 2>&1)"
want "dry-run names the superseded branch" "WOULD  sp-drysup" "$out"
want "and mentions the reason"             "superseded" "$out"
# After dry-run, the branch must still be there.
git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-drysup" \
    && ok "dry-run leaves the branch intact" \
    || bad "dry-run leaves the branch intact" "branch was deleted despite --dry-run"
drop_branch sp-drysup

# --------------------------------------------------------------------------------------
# SUPERSEDED ANCESTOR BRANCH (sp-bf31a). A superseded branch with zero commits ahead of
# the base (is-ancestor) was kept forever before the content_landed fix. With the fix,
# content_landed returns 0 for ancestor branches, so the Sending reaches the content-
# landed path and reaps the branch via SENT (not REAPED — the superseded block is not
# entered when content_landed returns 0).
# --------------------------------------------------------------------------------------
seed
git -C "$REPO" fetch -q origin
# Build a superseded branch with one commit, then fast-forward the base past it so
# the branch tip becomes an ancestor of origin/main (ahead=0, --is-ancestor).
git -C "$REPO" worktree add -q -b "spira/sp-anc" "$RUN/worktree/sp-anc" main
printf 'anc\n' > "$RUN/worktree/sp-anc/anc.txt"
git -C "$RUN/worktree/sp-anc" add -A
git -C "$RUN/worktree/sp-anc" commit -q -m "feat: sp-anc"
printf '{"id":"sp-anc","title":"sp-anc","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-anc","depends_on_id":"sp-goal","type":"parent-child"},{"issue_id":"sp-anc","depends_on_id":"sp-succ","type":"supersedes"}]}' \
    | testdb_seed
# Fast-forward origin/main past sp-anc by pushing sp-anc's commit there, then adding more.
git -C "$REPO" push -q origin "spira/sp-anc:main"
advance_base anc2.txt "extra commit past sp-anc"
git -C "$REPO" fetch -q origin
# Now: spira/sp-anc tip is an ancestor of origin/main, ahead=0, --is-ancestor true.
_anc_ahead="$(git -C "$REPO" rev-list --count "origin/main..spira/sp-anc" 2>/dev/null)"
if [ "${_anc_ahead:-?}" = 0 ]; then
    ok "sp-anc fixture: zero commits ahead (is-ancestor confirmed)"
else
    bad "sp-anc fixture" "expected 0 ahead, got ${_anc_ahead:-?}"
fi
git -C "$REPO" worktree remove --force "$RUN/worktree/sp-anc" >/dev/null 2>&1 || true
out="$(sending)"
echo "$out" | head -20 >&2

want   "the Sending reaps a superseded ancestor branch"  "SENT sp-anc"  "$out"
nowant "it does not keep it"                             "KEEP   sp-anc" "$out"
if git -C "$REPO" show-ref --verify --quiet "refs/heads/spira/sp-anc" 2>/dev/null; then
    bad "the superseded ancestor branch is gone" "spira/sp-anc still exists"
else
    ok "the superseded ancestor branch is gone"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
