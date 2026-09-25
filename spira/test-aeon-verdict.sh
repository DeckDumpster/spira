#!/usr/bin/env bash
#
# test-aeon-verdict.sh — the close verdict: a bead closed with nothing committed is
#   reopened, UNLESS superseded or its delivers:TYPE evidence verifies.
#
#   ./test-aeon-verdict.sh
#
# THE SEAM (UC-aeon-execution-13). aeon.sh's post-session check and sentinel CHECK5 used to
# carry two copies of the same delivers:TYPE case block, guarded only by
# test-delivers-parity.sh awk-extracting both and asserting they agreed. close_verdict,
# delivers_verdict and verdict_committed (lib.sh) are that logic extracted once; both
# callers now share it, so "aeon and sentinel decide delivers types identically" is true by
# construction and test-delivers-parity.sh is deleted (STRUCTURAL section below replaces
# its job: proving the callers actually use the shared functions, not a reintroduced copy).
#
# T1 — direct calls to close_verdict/delivers_verdict, no git, no aeon run.
# T2 — verdict_committed against real git fixtures, no aeon run (the landref walk).
# E2E — 2 rows only, through the real aeon.sh: a commit keeps a bead closed, no commit
#   reopens it. Every other case the original 13-run suite carried end to end (superseded,
#   delivers:beads, delivers:action, the deep-landref window) is now covered faster by T1
#   or T2 against the same functions aeon.sh and sentinel.sh actually call.
#
# THE BRIEF-RENDERING CASES BELOW (persona wall/no-wall, already-done mentions) are a
# different use case (UC-aeon-execution-07, brief rendering) that happens to live in this
# file; this bead does not touch them.
#
# defect: sp-dvlq
# covers: spira/aeon.sh spira/sentinel.sh spira/lib.sh UC-aeon-execution-13
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-verdict
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonverdict || { echo "test-aeon-verdict: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# A repository with a remote, because the base an aeon judges currency against is the
# remote-tracking ref.
ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
# Derive the copy set from a glob — not a hand-maintained list that drifts when lib.sh
# gains a new sourced dependency. Only production scripts; test-*.sh are excluded.
find "$HERE" -maxdepth 1 -name '*.sh' ! -name 'test-*.sh' -exec cp {} "$SPIRA_HOME/" \;
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"

# POSITIVE CONTROL (law-absence-needs-a-positive-control): prove the seam functions this
# whole suite exercises actually loaded, before any assertion trusts them.
# shellcheck disable=SC1091
. "$SPIRA_HOME/lib.sh"
for _fn in close_verdict delivers_verdict verdict_committed; do
    declare -f "$_fn" >/dev/null \
        || { printf 'test-aeon-verdict: %s is not defined after sourcing lib.sh\n' "$_fn" >&2; exit 1; }
done

# ==========================================================================================
echo "T1 — delivers_verdict, direct calls (no git, no aeon run):"
# ==========================================================================================
dv() { delivers_verdict "$@"; }  # dv <id> <delivers> <since> -> "ok|fail"

out="$(dv t action:action 0)";        is "action verifies"                 "1|" "$out"
out="$(dv t check:check 0)";          is "check with no command fails"     "0|delivers:check has no command — use delivers:check:<command>" "$out"
out="$(dv t check:true 0)";           is "check:true verifies"             "1|" "$out"
out="$(dv t check:false 0)";          is "check:false fails"               "0|delivers:check: command exited non-zero: false" "$out"
out="$(dv t bogustype:bogustype 0)";  is "an unrecognised type fails"      "0|delivers:bogustype is not a recognised type (beads, note, report, check, action)" "$out"
out="$(dv t note:note 0)";            is "note with no path fails"         "0|delivers:note has no file path — use delivers:note:/absolute/path" "$out"
out="$(dv t "note:$TMP/nonexistent-xyz" 0)"
is "note naming a missing file fails" "0|delivers:note: $TMP/nonexistent-xyz does not exist" "$out"

NOTEF="$TMP/note1"; : > "$NOTEF"; NOW="$(date +%s)"
out="$(dv t "note:$NOTEF" $((NOW + 100)))"
is "note predating the window fails" "0|delivers:note: $NOTEF exists but predates the window (mtime $(stat -c %Y "$NOTEF") <= $((NOW + 100)))" "$out"
out="$(dv t "note:$NOTEF" $((NOW - 100)))"
is "note inside the window verifies" "1|" "$out"

APPLIED="$TMP/applied.jsonl"
printf '{"bead": "not-this-one"}\n' > "$APPLIED"
out="$(dv t "report:$APPLIED" 0)"
is "applied.jsonl without this bead's record fails (identity check, not mtime)" \
   "0|delivers:report: $APPLIED has no record naming bead t" "$out"
printf '{"bead": "t"}\n' > "$APPLIED"
out="$(dv t "report:$APPLIED" 0)"
is "applied.jsonl naming this bead verifies" "1|" "$out"

testdb_reset
seed_bead() { printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[]}\n' "$1" | testdb_seed; }
seed_bead sp-tv-beads
bd -C "$SPIRA_DB" create --title c --type task --parent sp-tv-beads >/dev/null 2>&1
out="$(dv sp-tv-beads beads:beads 0)"
is "beads with a real child bead verifies"  "1|" "$out"

seed_bead sp-tv-nokids
out="$(dv sp-tv-nokids beads:beads 0)"
is "beads with no children fails" "0|delivers:beads declared but no child beads name sp-tv-nokids as source" "$out"

seed_bead sp-tv-evtonly
bd -C "$SPIRA_DB" create --title e --type event --parent sp-tv-evtonly \
    --event-category test.probe --event-actor "urn:test" --event-target sp-tv-evtonly >/dev/null 2>&1
out="$(dv sp-tv-evtonly beads:beads 0)"
is "beads with only an event-type child still fails (positive control: event children don't count)" \
   "0|delivers:beads declared but no child beads name sp-tv-evtonly as source" "$out"

# ==========================================================================================
echo
echo "T1 — close_verdict, direct calls (no git, no aeon run):"
# ==========================================================================================
cv() { close_verdict "$@"; }  # cv <id> <status> <superseded> <delivers> <committed> <since>

is "not closed at all is left alone"              "keep|committed" "$(cv t open 0 "" no 0)"
is "closed with a commit is left alone"           "keep|committed" "$(cv t closed 0 "" yes 0)"
is "superseded stays closed even with no delivers" "keep|superseded" "$(cv t closed 1 "" no 0)"
is "superseded stays closed even if delivers would have failed" \
   "keep|superseded" "$(cv t closed 1 bogus:bogus no 0)"
is "delivers verified stays closed"               "keep|delivers (action:action) verified" \
   "$(cv t closed 0 action:action no 0)"
is "delivers unverified is reopened as delivers-mismatch" \
   "reopen|delivers-mismatch|delivers:bogus is not a recognised type (beads, note, report, check, action)" \
   "$(cv t closed 0 bogus:bogus no 0)"
is "no delivers at all is reopened as closed-without-commit" \
   "reopen|closed-without-commit|" "$(cv t closed 0 "" no 0)"

# ==========================================================================================
echo
echo "STRUCTURAL — the shared functions replaced the duplicate case blocks (what"
echo "test-delivers-parity.sh used to guard, now true by construction):"
# ==========================================================================================
want "aeon.sh calls delivers_verdict (via close_verdict)" "close_verdict " "$(cat "$HERE/aeon.sh")"
want "sentinel.sh calls delivers_verdict directly"        "delivers_verdict " "$(cat "$HERE/sentinel.sh")"
nowant "aeon.sh carries no case \"\$_dtype\" of its own"     'case "$_dtype"' "$(cat "$HERE/aeon.sh")"
nowant "sentinel.sh carries no case \"\$_dtype\" of its own" 'case "$_dtype"' "$(cat "$HERE/sentinel.sh")"

# ==========================================================================================
echo
echo "T2 — verdict_committed against real git, no aeon run:"
# ==========================================================================================
T2ORIGIN="$TMP/t2origin.git"; git init -q --bare -b main "$T2ORIGIN"
T2REPO="$TMP/t2repo"; git clone -q "$T2ORIGIN" "$T2REPO" 2>/dev/null
git -C "$T2REPO" config user.email t@t; git -C "$T2REPO" config user.name t
printf 'seed\n' > "$T2REPO/f"; git -C "$T2REPO" add f
git -C "$T2REPO" commit -qm seed; git -C "$T2REPO" push -q origin main 2>/dev/null
T2MAP="$TMP/t2-repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$T2REPO" > "$T2MAP"

is "no commit anywhere reads as not committed" "no" \
   "$(SPIRA_REPO_MAP="$T2MAP" verdict_committed "$T2REPO" main sp-t2-nope 400)"

git -C "$T2REPO" checkout -q -b spira/sp-t2-branch
printf 'x\n' >> "$T2REPO/f"; git -C "$T2REPO" commit -qam "sp-t2-branch — the work"
is "a commit on the branch, not yet on the base, reads as committed" "yes" \
   "$(SPIRA_REPO_MAP="$T2MAP" verdict_committed "$T2REPO" spira/sp-t2-branch sp-t2-branch 400)"
git -C "$T2REPO" checkout -q main

# THE DEFECT THIS REPRODUCES (sp-fzfw). aeon.sh once walked -n 50 against the BRANCH only;
# CHECK5 walks -n SPIRA_VERDICT_WINDOW against the landing refs. A branch carrying leftover
# commits from a previous attempt sits deeper from the branch tip than from the base tip, so
# a bounded branch-only walk misses a commit the landref walk finds. Land the bead commit,
# add 2 more commits on the base, then build a branch with 3 "previous attempt" commits on
# top: branch-walk depth to the bead commit is 3 + 3 = 6, past a window of 5.
printf 'sp-t2-deep\n' >> "$T2REPO/f"; git -C "$T2REPO" commit -qam "sp-t2-deep — the work"
git -C "$T2REPO" push -q origin main 2>/dev/null
printf 'post1\n' >> "$T2REPO/f"; git -C "$T2REPO" commit -qam "post 1"; git -C "$T2REPO" push -q origin main 2>/dev/null
printf 'post2\n' >> "$T2REPO/f"; git -C "$T2REPO" commit -qam "post 2"; git -C "$T2REPO" push -q origin main 2>/dev/null
git -C "$T2REPO" checkout -q -b spira/sp-t2-deep
printf 'prev1\n' >> "$T2REPO/f"; git -C "$T2REPO" commit -qam "prev 1"
printf 'prev2\n' >> "$T2REPO/f"; git -C "$T2REPO" commit -qam "prev 2"
printf 'prev3\n' >> "$T2REPO/f"; git -C "$T2REPO" commit -qam "prev 3"
git -C "$T2REPO" checkout -q main
is "a commit past the branch-walk depth is still found via the landing refs" "yes" \
   "$(SPIRA_REPO_MAP="$T2MAP" verdict_committed "$T2REPO" spira/sp-t2-deep sp-t2-deep 5)"
is "the SAME window correctly says no for a bead with no commit at all (positive control)" "no" \
   "$(SPIRA_REPO_MAP="$T2MAP" verdict_committed "$T2REPO" main sp-t2-nowhere 5)"

# ==========================================================================================
echo
echo "E2E — the real aeon.sh, 2 rows only (everything else above is T1/T2 against the same"
echo "functions aeon.sh actually calls):"
# ==========================================================================================
# THE SHIM IS THE SESSION, running where the model would and finishing the bead the way the
# case under test needs it finished. The guard below is not decoration: conf.sh replaces
# $PATH, so a suite that tried to shim `claude` by PATH alone would run the real model
# against a real account, silently and at full cost.
BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-verdict: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"
shim() {   # shim <commit:0|1>
    printf '%s' "$1" > "$TMP/docommit"
    cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
if [ "$(cat "$TMP/docommit")" = 1 ]; then
    printf 'my work\n' >> f
    git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
fi
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
    chmod +x "$BIN/claude"
}
seed() {   # seed <id> [status]
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"%s","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "${2:-open}" "$_lbl" | testdb_seed
}
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; "$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1; }
field() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }
notes() { bd -C "$SPIRA_DB" show "$1" 2>/dev/null | tr '\n' ' '; }

echo
echo "closed WITH a commit naming the bead — the check is satisfied and does nothing:"
testdb_reset; seed sp-vd-1; shim 1; run_aeon
is     "the bead stays closed"        closed "$(field sp-vd-1 status)"
want   "and the verdict says it committed" "committed=yes" "$(cat "$TMP/out")"
nowant "with no reopen"               "REOPENED"           "$(cat "$TMP/out")"

echo
echo "closed with NOTHING committed — reopened, because closed is not landed:"
testdb_reset; seed sp-vd-2; shim 0; run_aeon
is   "the bead is open again"          open "$(field sp-vd-2 status)"
is   "and its claim is released"       ""   "$(field sp-vd-2 assignee)"
want "the verdict names the omission"  "REOPENED — closed with nothing committed" "$(cat "$TMP/out")"
want "and the bead carries the reason" "closed without a commit naming sp-vd-2"   "$(notes sp-vd-2)"

# ======================================================================================
echo
echo "a persona with a wall is told when it is killed, in the brief the model receives:"
# ======================================================================================
# THE DEFECT THIS REPRODUCES. A persona that declares FAYTH_TIMEOUT_SECONDS is killed on a
# clock from outside, and its brief asks it, if it cannot finish, to leave what it found in
# the graph rather than in a session that is about to end. It could not know when that was:
# four consecutive sessions on one incident were each killed within a second of the wall and
# left no commit and no bead between them. The wall is not the defect — it is what keeps a
# session from outliving the sweep that produces its work. Not being able to see it is.
#
# ASSERTED AGAINST THE PROMPT THE SHIM RECEIVES, which is the only place the claim is
# meaningful. A check on aeon.sh's source proves the token is mentioned; it cannot tell a
# deadline that renders as a time from one that renders as the empty string, and the empty
# string is what a brief with a `{{DEADLINE}}` in it silently becomes.
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{DEADLINE}}\n{{PARK}}\n' \
    > "$SPIRA_HOME/chamber/builder.md"
walled_fayth() {                # walled_fayth [seconds] — rewrite the fayth, with or without a wall
    { cat <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
      [ -n "${1:-}" ] && printf 'FAYTH_TIMEOUT_SECONDS=%s\n' "$1"
    } > "$SPIRA_HOME/chamber/builder.fayth"
}

# A NON-DEFAULT WALL. 480 is what the shipped Ops persona declares, so a deadline computed
# from a literal written into aeon.sh would pass against it and fail against nothing.
walled_fayth 300
testdb_reset; seed sp-vd-4; shim 1; run_aeon
prompt="$(cat "$TMP/prompt" 2>/dev/null)"
now="$(date +%s)"
nowant "no placeholder reaches the model" "{{" "$prompt"
want   "the brief says the session is killed" "This session is killed at" "$prompt"

# THE EPOCH IS THE PART THAT MATTERS: a clock time tells the aeon when it dies, the epoch is
# what lets it ASK how long is left at any point in the session rather than estimate. It is
# read back out of the prompt and required to fall inside the window the wall implies: after
# now, because the session is still running, and no later than now plus the wall, which is
# where a deadline anchored at the aeon's start can be and a fixed or fabricated one cannot.
epoch="$(printf '%s' "$prompt" | grep -oE 'echo \$\(\( [0-9]+ ' | grep -oE '[0-9]+' | head -1)"
if [ -n "$epoch" ] && [ "$epoch" -gt "$now" ] && [ "$epoch" -le $((now + 300)) ]; then
    ok "and carries a readable epoch inside the window the wall implies"
else
    bad "and carries a readable epoch inside the window the wall implies" \
        "got [$epoch], wanted between $now and $((now + 300))"
fi
left="$(printf '%s' "$prompt" | grep -oE '— [0-9]+ seconds from now' | grep -oE '[0-9]+' | head -1)"
if [ -n "$left" ] && [ "$left" -gt 0 ] && [ "$left" -le 300 ]; then
    ok "and a remaining count that spends what the claim already cost"
else
    bad "and a remaining count that spends what the claim already cost" \
        "got [$left], wanted 1..300"
fi

# THE OTHER HALF, and it is not decoration: a brief that renders `{{DEADLINE}}` as nothing at
# all would satisfy every assertion above if the persona simply had no wall. A persona
# without one must be told so, rather than told nothing.
walled_fayth
testdb_reset; seed sp-vd-5; shim 1; run_aeon
prompt="$(cat "$TMP/prompt" 2>/dev/null)"
nowant "a persona with no wall gets no placeholder either" "{{" "$prompt"
want   "and is told plainly that it has no clock" "no wall-clock deadline" "$prompt"
nowant "and is not given a deadline it does not have" "This session is killed at" "$prompt"

# ======================================================================================
echo
echo "the brief tells the aeon to use bd supersede rather than close when work is already done:"
# ======================================================================================
# THE DEFECT THIS REPRODUCES (sp-0gne). An aeon that finds the work already done closes
# with "already done" in the reason. The sentinel reads the commit graph, not the close
# reason: that close is indistinguishable from a failed attempt — the bead is reopened and
# charged. The fix: state the machine-readable path in the prompt the aeon receives, before
# it acts. The instruction appeared in the REOPEN NOTE, which is too late — it arrives after
# the attempt has been charged, and the next aeon starts from the prompt, not from that note.
#
# ASSERTED AGAINST THE PROMPT THE SHIM RECEIVES. A grep on aeon.sh proves the string is in
# the source; it cannot prove the string survives template rendering. ALREADY_DONE_BRIEF is
# appended to FULL unconditionally for every bead regardless of persona, so the prompt from
# the previous run contains it.
want "the brief tells aeons to use bd supersede for already-done work" \
     "bd supersede" "$prompt"
want "and to verify the successor actually landed first" \
     "Verify the successor actually landed" "$prompt"
nowant "no unreplaced placeholder reaches the model" "{{" "$prompt"

tl_summary
