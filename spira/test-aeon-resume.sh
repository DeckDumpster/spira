#!/usr/bin/env bash
#
# test-aeon-resume.sh — brief rendering: render_resume_brief / render_slain_brief / (see
#   test-session-yield-headless.sh and test-session-result-fields.sh for their lib.sh
#   neighbours). When a
#   branch carries prior commits, aeon.sh includes a RESUME_BRIEF in the model's prompt so it
#   resumes the existing work rather than restarting from scratch; when the last commit is a
#   slay.sh wip salvage, a SLAIN_BRIEF tells it to review that commit first.
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
# defect: sp-2e4v
# covers: spira/aeon.sh spira/lib.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-aeon-resume.sh"

rrb() {   # rrb <branch> <work> <count> <log> -> render_resume_brief's own output
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; render_resume_brief "$2" "$3" "$4" "$5"' \
        _ "$HERE" "$1" "$2" "$3" "$4" 2>/dev/null
}
rsb() {   # rsb <last-subject> <count> <base> <slay-when> <diffstat> <logf> -> render_slain_brief's output
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; render_slain_brief "$2" "$3" "$4" "$5" "$6" "$7"' \
        _ "$HERE" "$1" "$2" "$3" "$4" "$5" "$6" 2>/dev/null
}
rdb() {   # rdb <at> <now> -> render_deadline_brief's output
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; render_deadline_brief "$2" "$3"' \
        _ "$HERE" "$1" "$2" 2>/dev/null
}

# ===========================================================================================
echo
echo "T1: render_resume_brief <branch> <work> <count> <log>"
# ===========================================================================================
out="$(rrb spira/sp-x /work 0 '')"
is   "zero commits: RESUME_BRIEF is empty (positive control)" "" "$out"
out="$(rrb spira/sp-x /work '?' '')"
is   "unreadable count ('?'): RESUME_BRIEF is empty" "" "$out"

out="$(rrb spira/sp-x /work 2 '  abc123 sp-x — part 1
  def456 sp-x — part 2')"
want "prior commits: RESUME_BRIEF present"        "Prior work on this branch" "$out"
want "prior commits: branch named"                "\`spira/sp-x\` carries"    "$out"
want "prior commits: count is 2"                   "**2** commit(s)"          "$out"
want "prior commits: log block present"             "abc123 sp-x — part 1"     "$out"
want "prior commits: git log instruction names the worktree" "git -C /work log --oneline" "$out"
want "prior commits: resume instruction present"    "do not redo work that is already committed" "$out"

out="$(rrb spira/sp-x /work 1 '  abc123 sp-x — only commit')"
want "one commit: singular form still uses the count" "**1** commit(s)" "$out"

# ===========================================================================================
echo
echo "T1: render_slain_brief <last-subject> <count> <base> <slay-when> <diffstat> <logf>"
# ===========================================================================================
out="$(rsb 'sp-x — normal commit' 1 main '' '' /tmp/x.log)"
is   "a regular commit subject: SLAIN_BRIEF is empty (positive control)" "" "$out"

out="$(rsb 'sp-x: wip — salvaged at slay (operator halted the session)' 2 main '2026-09-25 10:00:00' '1 file changed' /tmp/x.log)"
want "slain subject: SLAIN_BRIEF present"     "A previous attempt was slain" "$out"
want "slain: why extracted from the subject"  "operator halted the session"   "$out"
want "slain: when is threaded through"        "2026-09-25 10:00:00"           "$out"
want "slain: commit count threaded through"   "**2** commit(s) beyond"        "$out"
want "slain: base named"                      "beyond \`main\`"                "$out"
want "slain: diffstat included when given"    "(1 file changed)"              "$out"
want "slain: wip review instruction"          "salvaged wip commit"           "$out"
want "slain: transcript path present"         "\`/tmp/x.log\`"                 "$out"

out="$(rsb 'sp-x: wip — salvaged at slay (crash)' 1 main '' '' /tmp/x.log)"
nowant "slain: an empty diffstat adds no stray parentheses" "()" "$out"

# ===========================================================================================
echo
echo "T1: render_deadline_brief <at> <now>"
# ===========================================================================================
out="$(rdb '' "$(date +%s)")"
want "no deadline: walless text"    "no wall-clock deadline" "$out"
nowant "no deadline: no kill time"  "killed at"               "$out"

now=1900000000
at=$(( now + 600 ))
out="$(rdb "$at" "$now")"
want "walled: names the kill mechanism" "killed at" "$out"
want "walled: seconds remaining"        "600 seconds from now" "$out"
want "walled: the live-read snippet carries the real epoch" "echo \$(( $at - \$(date +%s) ))" "$out"

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
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-resume: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }
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
