#!/usr/bin/env bash
#
# test-cross-repo.sh — an aeon working a bead for a non-home repo cuts its worktree in
#   the right checkout, and commits land in that repo's history, not the home repo's.
#
#   ./test-cross-repo.sh
#
# THE DEFECT THIS PREVENTS. FAYTH_REPO was a constant per fayth, and every fayth in the
# chamber pointed at the home checkout. So a bead labeled repo:widget had no aeon that
# could open the file — the workspace was always the home repo, regardless of which
# repository the bead named. This is the integration test that proves the whole cross-repo
# flow works: bead -> repo: label -> repo-map -> second repo's worktree -> commit in second
# repo's history.
#
# THREE PROPERTIES, EACH A PAIR (law-absence-needs-a-positive-control):
#
#   1. The worktree is cut in the SECOND repo, not the home repo. Without a positive
#      control showing the commit lands in second but NOT in home, a check that always
#      used the home repo would be indistinguishable from one that is correct.
#
#   2. The worktree PATH is inside the second repo's object store: the work directory is a
#      worktree of second, not a worktree of home. A cross-repo path that shared home's
#      object store would silently put work on a branch that could never land in second.
#
#   3. The aeon log records the correct repo name. The log is what the operator reads when
#      diagnosing a stalled bead, so an incorrect record there misleads the diagnosis.
#
# TWO REPOS, because the property this test exists to prove is about the DIFFERENCE between
# them. A single-repo fixture proves nothing: the home repo IS the correct repo.
#
# A REAL bd ON A FIXTURE DATABASE, a shim for the agent (via SPIRA_AGENT), and real git repos
# with remotes — the claim is about worktree creation and commit ancestry, and a stub of
# either would be a second implementation of the thing in question.
#
# defect: sp-cross-repo
# covers: spira/aeon.sh spira/lib.sh
# hermetic-ok: uses a fixture database, local git repos only, no systemd or gh
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
testdb_require test-cross-repo
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up crossrepo || { echo "test-cross-repo: could not build a fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# TWO REPOS: the home repo the harness is installed in, and the "second" repo that the
# bead's repo: label names. A bead for "second" must never touch the home repo.
HOME_ORIGIN="$TMP/home-origin.git"; git init -q --bare -b main "$HOME_ORIGIN"
HOME_REPO="$TMP/home"; git clone -q "$HOME_ORIGIN" "$HOME_REPO" 2>/dev/null
git -C "$HOME_REPO" config user.email t@t; git -C "$HOME_REPO" config user.name t
printf 'home-seed\n' > "$HOME_REPO/home.txt"
git -C "$HOME_REPO" add home.txt; git -C "$HOME_REPO" commit -qm "home seed"
git -C "$HOME_REPO" push -q origin main 2>/dev/null

SECOND_ORIGIN="$TMP/second-origin.git"; git init -q --bare -b main "$SECOND_ORIGIN"
SECOND_REPO="$TMP/second"; git clone -q "$SECOND_ORIGIN" "$SECOND_REPO" 2>/dev/null
git -C "$SECOND_REPO" config user.email t@t; git -C "$SECOND_REPO" config user.name t
printf 'second-seed\n' > "$SECOND_REPO/second.txt"
git -C "$SECOND_REPO" add second.txt; git -C "$SECOND_REPO" commit -qm "second seed"
git -C "$SECOND_REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/harness"; mkdir -p "$SPIRA_HOME/chamber"
# Derive the copy set from a glob — not a hand-maintained list that drifts when lib.sh
# gains a new sourced dependency. Only production scripts; test-*.sh are excluded.
find "$HERE" -maxdepth 1 -name '*.sh' ! -name 'test-*.sh' -exec cp {} "$SPIRA_HOME/" \;
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true

export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
# POSITIVE CONTROL (law-absence-needs-a-positive-control): prove the fixture harness loads
# before any assertion runs.
bash -c ". \"$SPIRA_HOME/lib.sh\"" \
    || { printf 'test-cross-repo: fixture harness failed to source lib.sh — dependency missing?\n' >&2; exit 1; }

# THE REPO-MAP: two repos, neither the other's alias. "home" maps to the home checkout;
# "second" maps to the second checkout. The bead will carry repo:second.
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'home   | %s | push | origin/main | |\n' "$HOME_REPO"   > "$SPIRA_REPO_MAP"
printf 'second | %s | push | origin/main | |\n' "$SECOND_REPO" >> "$SPIRA_REPO_MAP"

# THE FAYTH: one persona, FAYTH_HEARTBEAT_SECONDS high so the heartbeat never fires during
# the test run.
cat > "$SPIRA_HOME/chamber/builder.fayth" <<'FAYTH'
FAYTH_NAME=builder
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,needs-operator"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
# The prompt template only needs enough fields for the shim to extract the bead id and
# the worktree path. {{REPO}} is the WORKTREE path, not the checkout — the aeon works
# there. {{BEAD_ID}} is the bead id.
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' \
    > "$SPIRA_HOME/chamber/builder.md"

# THE SHIM stands in for the model. It extracts the bead id and the worktree path from
# the prompt, makes a commit in the worktree naming the bead, then closes it.
#
# SPIRA_AGENT IS THE INJECTION POINT. The guard below ensures the real model can never
# run accidentally — conf.sh replaces PATH, so shimming via PATH alone would not reach it.
BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP SPIRA_DB
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-cross-repo: aeon.sh has no SPIRA_AGENT injection point — refusing to run real model" >&2; exit 1; }

cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
# Read the full prompt from stdin.
prompt="$(cat /dev/stdin)"
id="$(printf '%s' "$prompt"    | sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' | head -1)"
wt="$(printf '%s' "$prompt"    | sed -n 's/^work [^ ]* in \([^ ]*\) .*/\1/p' | head -1)"
if [ -z "$id" ] || [ -z "$wt" ]; then
    printf '{"type":"result","subtype":"success","is_error":false,"result":"shim: could not parse prompt","num_turns":1}\n'
    exit 0
fi
# Commit naming the bead in the worktree the aeon prepared.
printf '%s cross-repo work\n' "$id" >> "$wt/cross-repo.txt"
git -C "$wt" add cross-repo.txt
git -C "$wt" -c user.email=a@a -c user.name=aeon commit -qm "$id sp-cross-repo — work in second repo"
# Close the bead.
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
SHIM
chmod +x "$BIN/claude"

# SPIRA_HOME_REPO names the home repo in the map. Without it, lib.sh derives the home from
# the basename of SPIRA_REPO, and that must match a key in the repo-map.
export SPIRA_HOME_REPO=home SPIRA_SCOPE_LABEL=home

B() { bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys; d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

run_aeon() { rm -rf "$SPIRA_RUN/worktree"; \
    SPIRA_REPO="$HOME_REPO" SPIRA_HOME_REPO=home \
    "$SPIRA_HOME/aeon.sh" builder > "$TMP/aeon.out" 2>&1; }

seed() {
    testdb_reset
    B create "cross-repo work" --labels "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan},repo:second" >/dev/null 2>&1
}

echo "test-cross-repo.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL: cross-repo aeon commits in the second repo, not the home repo:"
# ======================================================================================
seed
BID="$(B list --status open --label "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan},repo:second" --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); print(d[0]["id"] if isinstance(d,list) and d else "")' 2>/dev/null)"

[ -n "$BID" ] || { bad "fixture: could not create bead"; printf '\n  %d passed, %d failed\n' "$pass" "$fail"; exit 1; }

run_aeon

# The bead is closed (the shim closes it).
is "the bead ends closed" closed "$(status_of "$BID")"

# The commit landed in the SECOND repo. Check ALL local refs, not just origin/main — the
# branch has not been pushed yet (that is the landing pass's job). The commit is on
# refs/heads/spira/$BID in the second repo's object store.
second_log="$(git -C "$SECOND_REPO" log --format='%s' --all 2>/dev/null)"
want "commit naming the bead is in second repo's history" "$BID" "$second_log"

# The home repo has NO such commit. Without this positive control, a check that always
# used the home repo would pass the first assertion (if second mirrored home's log).
home_log="$(git -C "$HOME_REPO" log --format='%s' --all 2>/dev/null)"
nowant "home repo has no commit naming the bead" "$BID" "$home_log"

# ======================================================================================
echo
echo "the branch lives in the second repo, not the home repo:"
# ======================================================================================
# After the aeon runs, spira/$BID is a branch in the second repo. The worktree path
# (under SPIRA_RUN) may have been pruned, but the branch ref remains in the checkout
# that owns its object store — that is what "worktree of second" means.
second_has_branch=no; git -C "$SECOND_REPO" show-ref --verify -q "refs/heads/spira/$BID" 2>/dev/null && second_has_branch=yes
home_has_branch=no; git -C "$HOME_REPO" show-ref --verify -q "refs/heads/spira/$BID" 2>/dev/null && home_has_branch=yes
is "second repo owns the spira branch" yes "$second_has_branch"
is "home repo does not own the spira branch" no "$home_has_branch"

# ======================================================================================
echo
echo "the aeon log records the correct repo name:"
# ======================================================================================
# The aeon's log() function writes to stdout (not to the LOGF bead-log which only holds
# the model output). The stdout was captured to $TMP/aeon.out by run_aeon above.
aeon_out="$(cat "$TMP/aeon.out" 2>/dev/null)"
want "log says works repo:second" "repo:second" "$aeon_out"
want "log shows the second repo path" "$SECOND_REPO" "$aeon_out"
nowant "log does not say it used the home repo" "works repo:home" "$aeon_out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
