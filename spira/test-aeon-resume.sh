#!/usr/bin/env bash
#
# test-aeon-resume.sh — brief rendering: when a branch carries prior commits, aeon includes
#   a RESUME_BRIEF in the model's prompt so it resumes the existing work rather than
#   restarting from scratch; when the last commit is a slay.sh wip salvage, a SLAIN_BRIEF
#   tells it to review that commit first.
#
# THE DEFECT THIS REPRODUCES. When a bead is closed and its branch fails the landing gate,
# the sentinel reopens the bead and summons a new aeon. That aeon inherits the branch with
# its prior commits, but the prompt contained no mention of those commits — so the model
# re-implemented the work from scratch, paid the full session cost again, and closed a bead
# that was already done. sp-2e4v measured 16 such beads in one day, $49 of $491 spent.
#
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control): the fresh/no-slain case must
# show the brief is ABSENT, beside the case that shows it PRESENT.
#
# RETIRED (sp-j89pd, wave 4.2): render_resume_brief, render_slain_brief and
# render_deadline_brief (lib.sh) had zero live callers — all three are now aeon::brief
# (aeon/src/brief.rs). Their direct-call T1 table is deleted with them; T2 (pure git, no
# brief rendering at all) and T3 (the real `aeon` binary, prompt checked end to end) are
# unaffected and stay.
#
# defect: sp-2e4v
# tier: T2
# covers: aeon/src/* landing-pass/src/* spira/lib.sh UC-aeon-execution-07
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-aeon-resume.sh"

# ===========================================================================================
echo
echo "T2: the commit count is read AFTER the rebase, over real git — no bd, no aeon run"
# ===========================================================================================
# THE BUG THIS GUARDS AGAINST. The count BASE..BRANCH is correct only once the branch sits on
# top of the current base. Before a rebase the count can include commits already on the base;
# an aeon told "5 commits from a prior session" before the rebase would be counting commits
# that no longer exist at the post-rebase tip.
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
R="$TMP/git-repo"; git init -q -b main "$R"
git -C "$R" config user.email t@t; git -C "$R" config user.name t
printf 'seed\n' > "$R/f"; git -C "$R" add f; git -C "$R" commit -qm seed
git -C "$R" branch -q spira/sp-count main
git -C "$R" checkout -q spira/sp-count
printf 'prior\n' >> "$R/f"; git -C "$R" commit -qam "sp-count — prior attempt"
git -C "$R" checkout -q main
printf 'newbase\n' > "$R/g"; git -C "$R" add g; git -C "$R" commit -qm "advance base"
before="$(git -C "$R" rev-list --count "main..spira/sp-count")"
git -C "$R" checkout -q spira/sp-count
git -C "$R" rebase -q main >/dev/null 2>&1
after="$(git -C "$R" rev-list --count "main..spira/sp-count")"
is "the count before the rebase is 1 (branch and base share only the seed)" "1" "$before"
is "the count after the rebase is still 1 (one real commit, replayed onto the new base)" "1" "$after"
is "the rebased branch now sits ON the new base" \
   "$(git -C "$R" rev-parse main)" "$(git -C "$R" merge-base main spira/sp-count)"

# ===========================================================================================
echo
echo "T3: the real wiring — a slain wip commit produces both briefs in a real prompt"
# ===========================================================================================
# testdb-mode: default (embedded).
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-resume
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonresume || { echo "test-aeon-resume: could not build a fixture database"; exit 1; }

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SPIRA_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
# The model session is restricted (sp-v62vn); the shim is a fixture — testlib aeon_fixture_agent.
aeon_fixture_agent "$BIN/claude"
# The lifecycle machine (testlib lc_aeon_mirror): since sp-v62vn the aeon's ready set is
# `spira-lc list` and its claim a Claim event; the stand-in tells the fixture's bd story in
# lifecycle terms, ahead of the tree's spira-lc on PATH.
lc_aeon_mirror "$TMP/lc"; export PATH="$TMP/lc:$PATH"
command -v aeon >/dev/null 2>&1 \
    || { echo "test-aeon-resume: aeon is not on PATH — refusing to run the real model" >&2; exit 1; }
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'prior work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"%s","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "${2:-open}" "$_lbl" | testdb_seed
}
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; aeon --home "$SPIRA_HOME" builder > "$TMP/out" 2>&1; }

testdb_reset; seed sp-ar-slain
git -C "$REPO" fetch -q origin main 2>/dev/null
git -C "$REPO" checkout -q -B spira/sp-ar-slain origin/main
printf 'real work\n' >> "$REPO/f"; git -C "$REPO" commit -qam "sp-ar-slain — the real work"
printf 'dirty state\n' >> "$REPO/f"
git -C "$REPO" commit -qam "sp-ar-slain: wip — salvaged at slay (operator halted the session)"
git -C "$REPO" checkout -q main
run_aeon
want "wiring: RESUME_BRIEF present in the real prompt" "Prior work on this branch"    "$(cat "$TMP/prompt")"
want "wiring: SLAIN_BRIEF present in the real prompt"  "A previous attempt was slain" "$(cat "$TMP/prompt")"
want "wiring: slain reason present"                    "operator halted the session"  "$(cat "$TMP/prompt")"

# PAIR: a regular prior commit must NOT show SLAIN_BRIEF.
testdb_reset; seed sp-ar-noslain
git -C "$REPO" fetch -q origin main 2>/dev/null
git -C "$REPO" checkout -q -B spira/sp-ar-noslain origin/main
printf 'normal work\n' >> "$REPO/f"; git -C "$REPO" commit -qam "sp-ar-noslain — normal commit"
git -C "$REPO" checkout -q main
run_aeon
nowant "wiring: regular commit gets no SLAIN_BRIEF" "A previous attempt was slain" "$(cat "$TMP/prompt")"

echo
echo "T3: stale local base — the branch is cut from fresh origin/main, not stale local main"
# ===========================================================================================
# The sentinel lands from a dedicated worktree and never fast-forwards the shared checkout,
# so origin/main advances without updating local main. aeon.sh must fetch from the remote
# before cutting the branch, so the new branch starts at the FRESH origin/main.
#
# The discriminator: git merge-base(branch, origin/main).
#   If cut from fresh origin/main (B):  merge-base = B  (origin/main is an ancestor)
#   If cut from stale local main (A):   merge-base = A  (diverged before origin/main)
# defect: sp-stale-base
testdb_reset; seed sp-ar-stalebase
SECOND="$TMP/second"; git clone -q "$ORIGIN" "$SECOND" 2>/dev/null
git -C "$SECOND" config user.email t@t; git -C "$SECOND" config user.name t
printf 'sentinel-landed\n' >> "$SECOND/g"; git -C "$SECOND" add g
git -C "$SECOND" commit -qm "sentinel: landed something — origin/main advances"
git -C "$SECOND" push -q origin main 2>/dev/null
local_main_sha="$(git -C "$REPO" rev-parse main)"  # stale — does not include the push above
run_aeon
fresh_origin="$(git -C "$REPO" rev-parse origin/main)"   # updated by the fetch inside aeon.sh
branch_base="$(git -C "$REPO" merge-base "spira/sp-ar-stalebase" origin/main 2>/dev/null)"
nowant "setup: local main must differ from origin/main for this test to mean anything" \
    "$fresh_origin" "$local_main_sha"
is "stale-local-base: branch is cut from fresh origin/main, not stale local main" \
    "$fresh_origin" "$branch_base"

tl_summary
