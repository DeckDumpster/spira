#!/usr/bin/env bash
#
# test-aeon-resume.sh — when a branch carries prior commits, aeon.sh includes a RESUME_BRIEF
# in the model's prompt so it resumes the existing work rather than restarting from scratch.
#
#   ./test-aeon-resume.sh
#
# THE DEFECT THIS REPRODUCES. When a bead is closed and its branch fails the landing gate,
# the sentinel reopens the bead and summons a new aeon. That aeon inherits the branch with
# its prior commits, but the prompt contained no mention of those commits — so the model
# re-implemented the work from scratch, paid the full session cost again, and closed a bead
# that was already done. sp-2e4v measured 16 such beads in one day, $49 of $491 spent.
#
# The fix: aeon.sh computes the commit count (BASE..BRANCH) AFTER rebasing, and when it is
# non-zero, inserts a RESUME_BRIEF section naming those commits. Zero commits (fresh branch)
# gets no brief.
#
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control): the fresh-branch case must
# show the brief is ABSENT, beside the prior-commits case that shows it is PRESENT, so the
# test is not vacuously passing against a RESUME_BRIEF written unconditionally.
#
# Driven through the REAL aeon.sh against a real bd, with a shim for the model that captures
# the prompt, because the assertion is about what text reaches the model and a mock of
# aeon.sh would test the mock (law-prefer-the-real-dependency).
#
# defect: sp-2e4v
# covers: spira/aeon.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-resume
TMP="$(mktemp -d)"
# EXIT handles normal exit and the explicit `exit` below. INT and TERM must call `exit` so
# that the script does not continue with $TMP deleted — suites.sh kills the process group
# on TERM when it detects orphaned background jobs, and a trap that omits `exit` deletes
# $TMP then walks forward, producing spurious "No such file or directory" failures that
# mask the real cause. sp-82gai: the original failure was the heartbeat sleep child in
# aeon.sh outliving cleanup; sp-6a72t and sp-1ux75 fixed that, but the guard stays so a
# future orphan produces a clean signal rather than a misleading one.
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT
trap 'testdb_drop; rm -rf "$TMP"; exit 143' TERM
testdb_up aeonresume || { echo "test-aeon-resume: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$SPIRA_HOME/"
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

# The template uses a minimal placeholder set; the RESUME_BRIEF is appended by aeon.sh
# outside the template, so the template does not need a {{RESUME}} token.
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' \
    > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-resume: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }

# The shim captures the full prompt and then closes the bead with a commit so aeon.sh's
# verdict step is satisfied and the test ends cleanly.
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
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; "$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1; }

# ======================================================================================
echo
echo "FRESH branch — no prior commits, RESUME_BRIEF must be absent:"
# ======================================================================================
testdb_reset; seed sp-ar-fresh
run_aeon
nowant "fresh branch: RESUME_BRIEF absent" \
    "Prior work on this branch" "$(cat "$TMP/prompt")"
nowant "fresh branch: no commit count in prompt" \
    "commit(s) from a previous session" "$(cat "$TMP/prompt")"

# ======================================================================================
echo
echo "PRIOR COMMITS on branch — RESUME_BRIEF must appear in the prompt:"
# ======================================================================================
# Plant two commits on the branch before aeon.sh runs, simulating a reopened bead.
testdb_reset; seed sp-ar-prior
bd -C "$SPIRA_DB" set-state sp-ar-prior "branch=spira/sp-ar-prior" >/dev/null 2>&1 || true
git -C "$REPO" fetch -q origin main 2>/dev/null
git -C "$REPO" checkout -q -B spira/sp-ar-prior origin/main
printf 'attempt1-a\n' >> "$REPO/f"; git -C "$REPO" commit -qam "sp-ar-prior — first attempt part 1"
printf 'attempt1-b\n' >> "$REPO/f"; git -C "$REPO" commit -qam "sp-ar-prior — first attempt part 2"
git -C "$REPO" checkout -q main
run_aeon
want "prior commits: RESUME_BRIEF present" \
    "Prior work on this branch" "$(cat "$TMP/prompt")"
want "prior commits: commit count in prompt" \
    "commit(s) from a previous session" "$(cat "$TMP/prompt")"
want "prior commits: count is 2" \
    "**2** commit(s)" "$(cat "$TMP/prompt")"
want "prior commits: git log instruction present" \
    "git -C" "$(cat "$TMP/prompt")"
want "prior commits: resume instruction present" \
    "do not redo work that is already committed" "$(cat "$TMP/prompt")"

# ======================================================================================
echo
echo "ONE PRIOR COMMIT — singular form, RESUME_BRIEF still appears:"
# ======================================================================================
testdb_reset; seed sp-ar-one
bd -C "$SPIRA_DB" set-state sp-ar-one "branch=spira/sp-ar-one" >/dev/null 2>&1 || true
git -C "$REPO" fetch -q origin main 2>/dev/null
git -C "$REPO" checkout -q -B spira/sp-ar-one origin/main
printf 'one-attempt\n' >> "$REPO/f"; git -C "$REPO" commit -qam "sp-ar-one — previous attempt"
git -C "$REPO" checkout -q main
run_aeon
want "one commit: RESUME_BRIEF present" \
    "Prior work on this branch" "$(cat "$TMP/prompt")"
want "one commit: count is 1" \
    "**1** commit(s)" "$(cat "$TMP/prompt")"

# ======================================================================================
echo
echo "PRIOR COMMITS PLUS BASE MOVED — RESUME_BRIEF after rebase:"
# ======================================================================================
# Push a new commit on main (base moved), then check that RESUME_BRIEF still fires
# after the rebase aligns the branch.
testdb_reset; seed sp-ar-rebased
bd -C "$SPIRA_DB" set-state sp-ar-rebased "branch=spira/sp-ar-rebased" >/dev/null 2>&1 || true
git -C "$REPO" fetch -q origin main 2>/dev/null
# Plant prior work on the branch at the current base
git -C "$REPO" checkout -q -B spira/sp-ar-rebased origin/main
printf 'prior\n' >> "$REPO/f"; git -C "$REPO" commit -qam "sp-ar-rebased — prior attempt"
git -C "$REPO" checkout -q main
# Advance main (base moves)
printf 'newbase\n' >> "$REPO/g"; git -C "$REPO" add g
git -C "$REPO" commit -qm "advance base"; git -C "$REPO" push -q origin main 2>/dev/null
run_aeon
want "rebased: RESUME_BRIEF present" \
    "Prior work on this branch" "$(cat "$TMP/prompt")"
want "rebased: count is 1 after rebase" \
    "**1** commit(s)" "$(cat "$TMP/prompt")"

# ======================================================================================
echo
echo "STALE LOCAL BASE — branch is cut from fresh origin/main, not stale local main:"
# ======================================================================================
# The sentinel lands from a dedicated worktree and never fast-forwards the shared
# checkout, so origin/main advances without updating local main. This reproduces the
# pattern: a second clone pushes to origin, leaving local main behind. aeon.sh must
# fetch from the remote before cutting the branch, so the new branch starts at the
# FRESH origin/main rather than the stale local ref.
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
# Prove the setup is valid: local main must differ from origin/main for this test to mean
# something. If they are equal, the stale-vs-fresh distinction collapses and silence
# passes vacuously.
[ "$local_main_sha" != "$fresh_origin" ] \
    && ok "setup: local main is behind origin/main" \
    || bad "setup: local main is behind origin/main" "local and remote are the same commit — stale setup failed"
[ "$branch_base" = "$fresh_origin" ] \
    && ok "stale-local-base: branch is cut from fresh origin/main, not stale local main" \
    || bad "stale-local-base: branch is cut from fresh origin/main, not stale local main" \
       "merge-base is [$branch_base], wanted fresh origin/main [$fresh_origin]"

# ======================================================================================
echo
echo "SLAIN WIP COMMIT — SLAIN_BRIEF must appear when the last commit is a slay wip:"
# ======================================================================================
# Plant a normal commit and then a wip salvage commit (the shape slay.sh produces),
# simulating a bead whose prior session was slain with uncommitted work.
testdb_reset; seed sp-ar-slain
bd -C "$SPIRA_DB" set-state sp-ar-slain "branch=spira/sp-ar-slain" >/dev/null 2>&1 || true
git -C "$REPO" fetch -q origin main 2>/dev/null
git -C "$REPO" checkout -q -B spira/sp-ar-slain origin/main
printf 'real work\n' >> "$REPO/f"; git -C "$REPO" commit -qam "sp-ar-slain — the real work"
printf 'dirty state\n' >> "$REPO/f"
git -C "$REPO" commit -qam "sp-ar-slain: wip — salvaged at slay (operator halted the session)"
git -C "$REPO" checkout -q main
run_aeon
want "slain: SLAIN_BRIEF present"     "A previous attempt was slain"   "$(cat "$TMP/prompt")"
want "slain: why in brief"            "operator halted the session"     "$(cat "$TMP/prompt")"
want "slain: wip review instruction"  "salvaged wip commit"             "$(cat "$TMP/prompt")"
want "slain: transcript path present" ".log"                            "$(cat "$TMP/prompt")"
want "slain: commit count in brief"   "commit(s) beyond"                "$(cat "$TMP/prompt")"

# PAIR: a regular prior commit must NOT show SLAIN_BRIEF.
testdb_reset; seed sp-ar-noslain
bd -C "$SPIRA_DB" set-state sp-ar-noslain "branch=spira/sp-ar-noslain" >/dev/null 2>&1 || true
git -C "$REPO" fetch -q origin main 2>/dev/null
git -C "$REPO" checkout -q -B spira/sp-ar-noslain origin/main
printf 'normal work\n' >> "$REPO/f"; git -C "$REPO" commit -qam "sp-ar-noslain — normal commit"
git -C "$REPO" checkout -q main
run_aeon
nowant "regular commit: SLAIN_BRIEF absent" \
    "A previous attempt was slain" "$(cat "$TMP/prompt")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
