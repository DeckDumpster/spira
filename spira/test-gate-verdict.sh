#!/usr/bin/env bash
#
# test-gate-verdict.sh — the gate computes a verdict once, and reuses it only for the same
# question.
#
#   ./test-gate-verdict.sh
#
# WHY THIS SUITE EXISTS AT ALL. Every bead paid the gate at least twice — an aeon runs it
# before it closes, and the landing pass runs it again on the same tree minutes later — and
# that second run was the whole of the close-to-landed latency and most of the contention on
# the gate's single worktree. The verdict cache removes it. What a cache can do wrong is
# answer a question it was never asked, and here that means landing a branch on a trial
# nobody ran: the most expensive failure this harness has, and a silent one, because a reused
# verdict and a real pass are the same exit status.
#
# SO EVERY CASE IS TWO-SIDED. A suite that only checked "the second run was fast" would pass
# just as well against a gate that had quietly stopped running anything
# (law-absence-needs-a-positive-control), so each case here first proves the repository's own
# gate command CAN run and then requires it to have run, or not to have, by counting its
# invocations. The command is also armed to FAIL while the cache is expected to hold, which
# is the strong form: a run that reached the command at all is a red, not a slower green.
#
# The repository, its remote and its branches are real git, and the gate under test is a copy
# of this harness rather than the installed one — the harness's own bytes are part of the
# key, so a case about a changed harness has to be able to change one.
#
# defect: sp-0v8
# tier: T1
# covers: spira/gate.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
isnt()   { [ "$2" != "$3" ] && ok "$1" || bad "$1" "did not want [$3]"; }

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
MAP="$TMP/repo-map"; VDIR="$TMP/verdicts"; GATELOG="$TMP/gate.log"; HOMEDIR="$TMP/home"
RUNS="$TMP/invocations"; TRIP="$TMP/trip"
mkdir -p "$RUN/worktree" "$HOMEDIR" "$SH"
: > "$RUNS"

# THE GATE UNDER TEST IS A COPY, and it is the copy's own bytes that go into the key. lib.sh
# and conf.sh travel with it because lib.sh refuses to run without conf.sh beside it;
# exclude.sh and skew.sh because the gate fails closed on their absence and hashes both;
# yield.sh because the gate records what it was worth on every way out, and a suite that left
# it behind would be exercising a path the real gate never takes.
cp "$HERE/gate.sh" "$HERE/gate-lib.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/exclude.sh" "$HERE/skew.sh" \
   "$HERE/yield.sh" "$SH/"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
printf 'base\n' > "$REPO/marker"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin

BR=spira/sp-v1
W="$TMP/work"
git -C "$REPO" worktree add -q -b "$BR" "$W" origin/main
printf 'one\n' > "$W/f1.txt"
git -C "$W" add -A; git -C "$W" commit -q -m "feat: sp-v1 — work"

# THE REPOSITORY'S OWN GATE COMMAND, and it is the instrument. It records that it ran, and it
# fails whenever the trip file exists — so "the cache held" is not inferred from a log line
# the gate writes about itself, but from the command's own count and from a red that does not
# appear.
setcmd() {               # setcmd [extra-shell-prefix]
    printf 'repo | %s | push | origin/main |  | %s%s\n' \
        "$REPO" "${1:-}" "printf 'ran\\n' >> $RUNS; [ ! -e $TRIP ]" > "$MAP"
}
setcmd
runs() { wc -l < "$RUNS" | tr -d ' '; }

# The gate's whole environment, named. Ambient configuration decides verdicts, and a suite
# that inherited a real spira.conf would be asserting about one box
# (law-gates-run-in-a-clean-environment). SPIRA_VERDICT_TTL is pinned to a NON-DEFAULT
# throughout: asserting against the shipped 86400 would pass just as well if the number were
# written into gate.sh, which is the thing a configuration key exists to stop.
TTL=600
rungate() {              # rungate [VAR=VAL ...] -> the gate's own exit status, output on stdout
    env -i HOME="$HOMEDIR" PATH="/usr/bin:/bin" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_REPO_MAP="$MAP" SPIRA_GATE_LOG="$GATELOG" \
        SPIRA_VERDICTS="$VDIR" SPIRA_VERDICT_TTL="$TTL" \
        "$@" bash "$SH/gate.sh" "$BR" repo 2>&1
}
entries() { ls -1 "$VDIR" 2>/dev/null | wc -l | tr -d ' '; }
newest()  { ls -1t "$VDIR"/* 2>/dev/null | head -1; }

echo "test-gate-verdict.sh — one verdict per question, and no verdict for a different one"

# --------------------------------------------------------------------------------------
# THE POSITIVE CONTROL. Before any claim that the gate did not run, it has to be shown
# running: one invocation of the repository's command, a PASS, and an entry on disk.
# --------------------------------------------------------------------------------------
out="$(rungate)"; rc=$?
is  "the first gate on a branch runs the repository's own command" 1 "$(runs)"
is  "and it passes"                                               0 "$rc"
want "and says so"                       "VERDICT=PASS reason=pass" "$out"
is  "and records one verdict"                                     1 "$(entries)"

# --------------------------------------------------------------------------------------
# THE CASE THE CACHE EXISTS FOR. The landing pass asks the identical question minutes later.
# The command would now FAIL if it were reached, so a pass here can only be the reused one.
# --------------------------------------------------------------------------------------
: > "$TRIP"
out="$(rungate)"; rc=$?
is  "the same tree is not judged twice"    1 "$(runs)"
is  "and the second caller still gets a verdict" 0 "$rc"
want "and it is named as a reused one" "VERDICT=PASS reason=cached" "$out"
# The meter row too: a reused verdict that wrote no row would be indistinguishable in the log
# from a gate that was never called, which is the shape every silent-skip bug takes.
want "and the reuse is metered like any other run" "rc=0 cached" "$(cat "$GATELOG")"

# --------------------------------------------------------------------------------------
# THE BASE MOVED, WHICH IS NOT A DIFFERENT QUESTION. This gate judges the branch's own tree,
# detached and alone; the base's part in that trial is the changed-file list and nothing else.
# The branch here is NOT rebased onto the new base, which is the shape that matters: a retry,
# a second pass, a caller that gates a tree it has already gated. Keying on the base commit
# would make every landing in the repository retire that verdict for a reason the trial never
# read. A branch that IS rebased gets the base's content into its own tree, and the case below
# covers that from the other side — the tree moved, so the verdict goes.
# --------------------------------------------------------------------------------------
git -C "$REPO" checkout -q main
printf 'moved\n' > "$REPO/other.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "someone else landed"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
git -C "$REPO" checkout -q --detach origin/main
out="$(rungate)"; rc=$?
is  "a landing on the base does not invalidate the verdict" 1 "$(runs)"
is  "and the reused verdict is still a pass"                0 "$rc"

# --------------------------------------------------------------------------------------
# THE TREE CHANGED, WHICH IS. This is the half that a broken cache gets wrong silently, and
# the trip file is what makes the failure loud: if the command is reached it fails, so a
# green here would mean work landing on a trial that was never run against it.
# --------------------------------------------------------------------------------------
before="$(runs)"
printf 'two\n' > "$W/f2.txt"
git -C "$W" add -A; git -C "$W" commit -q -m "feat: sp-v1 — more work"
out="$(rungate)"; rc=$?
isnt "a changed tree is judged again"  "$before" "$(runs)"
isnt "and its verdict is not the old one"     0   "$rc"

rm -f "$TRIP"
before="$(runs)"; entries_before="$(entries)"
out="$(rungate)"; rc=$?
is  "the changed tree passes on its own account"    0 "$rc"
isnt "having actually run the command" "$before" "$(runs)"
isnt "and it is recorded separately"   "$entries_before" "$(entries)"

# --------------------------------------------------------------------------------------
# A DIFFERENT COMMAND IS A DIFFERENT QUESTION, and so is a different harness. Both are in the
# key because both decide what a pass MEANS: reusing across either would be answering with a
# verdict from a gate that no longer exists.
# --------------------------------------------------------------------------------------
: > "$TRIP"
before="$(runs)"
setcmd ': ; '
out="$(rungate)"; rc=$?
isnt "a changed gate command is judged again" "$before" "$(runs)"
isnt "and does not inherit the old verdict"        0     "$rc"

setcmd                                      # back to the command that was already judged
before="$(runs)"
out="$(rungate)"; rc=$?
is  "and restoring it finds that verdict again" "$before" "$(runs)"
is  "which is still a pass"                          0    "$rc"

before="$(runs)"
printf '\n# a change to the harness itself\n' >> "$SH/exclude.sh"
out="$(rungate)"; rc=$?
isnt "a changed harness is judged again" "$before" "$(runs)"
isnt "and does not inherit the old verdict"   0    "$rc"

# --------------------------------------------------------------------------------------
# ONLY A PASS IS CACHED. A red is not a pure function of the tree — a flaky suite fails once
# and passes next time — so caching one would pin a flake to a branch permanently, which is
# far worse than paying for a re-run. The trip file makes every run below red.
# --------------------------------------------------------------------------------------
cp "$HERE/exclude.sh" "$SH/exclude.sh"
# A COMMAND NOTHING HAS PASSED UNDER, so the red below is a red and not a reused pass from
# earlier in this suite. `:` takes an argument and does nothing with it, which moves the key
# without changing what the command does — a `#` would comment out the rest of the line and
# turn the failing command into a passing one.
setcmd ': only-a-pass-is-cached; '
entries_before="$(entries)"; before="$(runs)"
rungate >/dev/null 2>&1
isnt "a red gate reaches the command"        "$before" "$(runs)"
is   "and records no verdict"      "$entries_before" "$(entries)"
before="$(runs)"
rungate >/dev/null 2>&1
isnt "so the next caller runs the suites again" "$before" "$(runs)"

# --------------------------------------------------------------------------------------
# AND A VERDICT EXPIRES. The key names everything the verdict depended on except the box —
# the toolchain, what was installed beside it, what the network answered — and those drift
# while the key stands still. The age is the entry's own `at=`, so this is a fixture and not
# a wait.
# --------------------------------------------------------------------------------------
rm -f "$TRIP"
rungate >/dev/null 2>&1                     # a fresh pass to age
ENTRY="$(newest)"
: > "$TRIP"
before="$(runs)"
out="$(rungate)"; rc=$?
is  "the fresh verdict is reused"  "$before" "$(runs)"
is  "and is a pass"                     0    "$rc"

sed -i "s/^at=.*/at=$(( $(date +%s) - TTL - 60 ))/" "$ENTRY"
before="$(runs)"
out="$(rungate)"; rc=$?
isnt "a verdict older than SPIRA_VERDICT_TTL is not reused" "$before" "$(runs)"

# THE OTHER DIRECTION OF THE SAME KEY, which is what makes the case above about the TTL and
# not about the clock: the identical entry, at the identical age, is reused when the TTL is
# raised past it. A suite that only showed the refusal would pass against a gate that had
# stopped reusing anything at all.
before="$(runs)"
out="$(TTL=$(( TTL + 3600 )) rungate)"; rc=$?
is  "the same entry is reused under a longer TTL" "$before" "$(runs)"
is  "and is still a pass"                              0    "$rc"

# FAIL CLOSED ON ITS OWN BOOKKEEPING. An entry with no timestamp cannot be aged, and an
# un-ageable verdict is one whose freshness nobody can vouch for — so it is refused, and the
# cost is one gate that would have been skipped.
sed -i '/^at=/d' "$ENTRY"
before="$(runs)"
out="$(rungate)"; rc=$?
isnt "an entry with no timestamp is not reused" "$before" "$(runs)"

# --------------------------------------------------------------------------------------
# A DEADLINE IS NOT A RED (sp-p4rl). A gate killed at its budget exits 124, and the verdict
# is NO_VERDICT/timeout — not FAIL — because the machinery ran out of time, not because the
# branch broke anything. The message names both the budget and the command, so the reader
# knows what to raise and what was running.
# --------------------------------------------------------------------------------------
rm -f "$TRIP"
setcmd 'sleep 10; '
before="$(runs)"
out="$(rungate SPIRA_GATE_TIMEOUT=1)"; rc=$?
is  "a gate killed at its deadline exits NO_VERDICT"        75  "$rc"
want "and says timeout"                              "reason=timeout" "$out"
want "and names the budget"                          "killed at 1s"   "$out"
want "and names the command"                         "command:"       "$out"
want "and the verdict line says NO_VERDICT"          "VERDICT=NO_VERDICT" "$out"

# AND IT IS NOT CACHED. A timeout is not evidence the branch is broken, and it is not evidence
# the branch is clean either — caching it would pin a NO_VERDICT to a branch that might pass
# on its next run with a higher budget. Only a PASS is cached.
entries_before="$(entries)"
is  "a timeout records no verdict"     "$entries_before" "$(entries)"

setcmd
tl_summary
