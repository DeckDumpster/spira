#!/usr/bin/env bash
#
# test-aeon-worktree-collision.sh — aeon.sh never puts a second live session in a worktree
#                                    another bead already holds (law-one-aeon-one-worktree).
#
#   ./test-aeon-worktree-collision.sh
#
# THE DEFECT THIS REPRODUCES. A bead's branch is recorded state (bead_branch, lib.sh), and a
# split that inherits a label handed every child the SAME recorded branch as its parent —
# sp-zs04v.2/.5/.6 all carried branch:spira/sp-zs04v, the PARENT's branch. `git worktree add`
# refuses a second worktree on one branch, and aeon.sh treated that refusal as a thing to work
# around: it found the other worktree and adopted it, putting a second live session in a
# directory a sibling was actively using. Three sessions worked one worktree on sp-zs04v; a
# sibling's git reset, cleaning up its own debug commit, silently dropped another session's
# already-committed fix — caught only because that session diffed its tree before closing.
#
# TWO CASES, positive and negative (law-absence-needs-a-positive-control):
#   1. Two beads recorded onto the SAME branch: the second bead's summon must FAIL, and must
#      never create a worktree for it — not merely log something.
#   2. One bead whose OWN branch is checked out at a path that isn't its canonical worktree
#      (a leftover from a previous naming convention, or a hand-made tree): the summon must
#      move that tree ASIDE — never delete it — and cut a fresh worktree at the canonical path.
#
# Driven through the REAL aeon.sh against a real bd and a real git repository, because the
# assertion is about `git worktree add`'s actual refusal and aeon.sh's actual response to it
# (law-prefer-the-real-dependency) — a stub of either would test the stub.
#
# defect: sp-gseub
# covers: spira/aeon.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]" ;; esac; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-worktree-collision
TMP="$(mktemp -d)"
# See test-aeon-resume.sh: INT/TERM must call exit explicitly or suites.sh's orphan-kill
# leaves the trap running forward against a deleted $TMP.
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT
trap 'testdb_drop; rm -rf "$TMP"; exit 143' TERM
testdb_up aeonwtcollision || { echo "test-aeon-worktree-collision: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$HERE/suite-covers.sh" "$SPIRA_HOME/"
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
# The template uses a minimal placeholder set; aeon.sh's own die()/log() calls before the
# session starts never reach the model at all, so no {{RESUME}}-style token is needed here.
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' \
    > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-worktree-collision: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }

# The shim: commit trivial work and close whichever bead the prompt names, so a session that
# reaches the model at all ends cleanly.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'work\n' >> f
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
run_aeon() { "$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1; }

# ======================================================================================
echo
echo "CASE 1 (positive control): two beads recorded onto the SAME branch — the second's summon FAILS:"
# ======================================================================================
testdb_reset
rm -rf "$SPIRA_RUN/worktree"

seed sp-cw-a
run_aeon
[ -e "$SPIRA_RUN/worktree/sp-cw-a/.git" ] \
    && ok "setup: bead A got its own worktree, still registered after closing" \
    || bad "setup: bead A got its own worktree" "missing $SPIRA_RUN/worktree/sp-cw-a/.git; out=$(cat "$TMP/out" 2>/dev/null)"

seed sp-cw-b
bd -C "$SPIRA_DB" set-state sp-cw-b "branch=spira/sp-cw-a" >/dev/null 2>&1
run_aeon
out2="$(cat "$TMP/out" 2>/dev/null)"

if [ -e "$SPIRA_RUN/worktree/sp-cw-b" ]; then
    bad "case 1: second bead never got a workspace" "found $SPIRA_RUN/worktree/sp-cw-b"
else
    ok "case 1: second bead never got a workspace"
fi
want "case 1: log names the branch"     "spira/sp-cw-a"                  "$out2"
want "case 1: log names the holder id"  "sp-cw-a"                        "$out2"
want "case 1: log names the holder path" "$SPIRA_RUN/worktree/sp-cw-a"   "$out2"

# The other bead's worktree must be untouched — refusal, not repair.
[ -e "$SPIRA_RUN/worktree/sp-cw-a/.git" ] \
    && ok "case 1: bead A's worktree is untouched by B's refused summon" \
    || bad "case 1: bead A's worktree is untouched by B's refused summon" "gone"

# ======================================================================================
echo
echo "CASE 2 (the legitimate case): this bead's OWN branch, checked out at a path that is"
echo "                              not its canonical worktree — moved aside, fresh tree cut:"
# ======================================================================================
testdb_reset
rm -rf "$SPIRA_RUN/worktree"

# A previous naming convention (or a hand-made tree) held this bead's own branch outside
# $SPIRA_RUN/worktree entirely — nothing under that root names this path, so it cannot be
# mistaken for a live sibling's canonical directory.
OLDPATH="$TMP/handmade/sp-cw-c-legacy"
mkdir -p "$(dirname "$OLDPATH")"
git -C "$REPO" worktree add -q -b spira/sp-cw-c "$OLDPATH" origin/main
printf 'old-attempt\n' > "$OLDPATH/g"
git -C "$OLDPATH" add g
git -c user.email=t@t -c user.name=t -C "$OLDPATH" commit -qm "sp-cw-c — from a previous path"

seed sp-cw-c
run_aeon
out3="$(cat "$TMP/out" 2>/dev/null)"

[ -e "$SPIRA_RUN/worktree/sp-cw-c/.git" ] \
    && ok "case 2: a fresh worktree was cut at the canonical path" \
    || bad "case 2: a fresh worktree was cut at the canonical path" "missing; out=$out3"
[ -d "$OLDPATH.prior" ] \
    && ok "case 2: the previous-path tree was moved aside, not deleted" \
    || bad "case 2: the previous-path tree was moved aside, not deleted" "no $OLDPATH.prior; out=$out3"
[ -e "$OLDPATH" ] \
    && bad "case 2: nothing remains registered at the old path" "still present" \
    || ok "case 2: nothing remains registered at the old path"
# The prior commit survives at the new (moved-aside) location — nothing deleted.
want "case 2: the prior attempt's commit survived the move" \
    "from a previous path" \
    "$(git -C "$OLDPATH.prior" log --format='%s' -5 2>/dev/null)"
want "case 2: bead C's own summon still reached the model and closed the bead" \
    "sp-cw-c" \
    "$(cat "$TMP/prompt" 2>/dev/null)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
