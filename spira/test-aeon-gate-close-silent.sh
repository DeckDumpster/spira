#!/usr/bin/env bash
#
# test-aeon-gate-close-silent.sh — gate-run.sh --status keeps its verdicts apart: a verdict
#                                   recorded for another tree answers 4, a run that died
#                                   without recording one answers 5 — never the 3 of "never
#                                   gated" or the 1 of a real FAIL.
#
# WHAT THIS SUITE USED TO BE. It drove the aeon's teardown CLOSED branch end to end: a session
# that closed its own bead in bd while the gate said PASS (0), still running (2), FAIL (1),
# no run (3), stale (4) or died (5), and the aeon's note, reopen, handoff-to-the-landing-pass,
# in-session fast tier and open+spira-submitted conversion that followed. sp-v62vn retired the
# unrestricted session: every session the aeon launches is restricted and hands its bead on
# only through the work verbs, which teardown reads as submitted — the closed branch
# (aeon decide::builder_closed: a restricted session "never took this path and still does
# not") is reached by no session, so every one of those rows asserted a path nothing runs
# and is deleted, not rewritten. UC-aeon-execution-15 is marked uncovered in
# docs/test-plan/aeon-execution.toml with that reason.
#
# WHAT IS LEFT is the part that never depended on the aeon: gate-run.sh's own --status codes
# (sp-7uah8's stale-key 4, sp-k7klr's died-without-verdict 5), against the real gate-run.sh.
#
# tier: T2
# covers: spira/gate-run.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-gate-close-silent
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeongcls || { echo "test-aeon-gate-close-silent: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null
export REPO

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
# conf.d IS COPIED IN (matching test-aeon-sweep.sh, test-aeon-world-stop.sh, ...): aeon's
# own in-process config registry (spira_config::resolve, aeon::conf::merge_resolved_config)
# derives conf.d from THIS --home and now REFUSES to start if it is missing (sp-1cdgq) --
# a --home with no conf.d used to resolve silently to nothing instead of refusing.
cp -r "$HERE/conf.d" "$SPIRA_HOME/"
printf '. "%s/lib.sh"\n' "$HERE" > "$SPIRA_HOME/lib.sh"   # the aeon binary sources <home>/lib.sh; this is the real one, as aeon.sh sourced it
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | queue | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
echo "test-aeon-gate-close-silent.sh"

# ======================================================================================
echo
echo "gate-run.sh itself (not the stub): a verdict recorded for one tree answers 4, not"
echo "3, once the base moves and the branch is rebased onto it — the two conditions"
echo "gate-run.sh --status used to collapse into one exit code (sp-7uah8):"
# ======================================================================================
BR2="spira/sp-gcs-stalekey"
git -C "$REPO" fetch -q origin
git -C "$REPO" checkout -q -B "$BR2" origin/main >/dev/null 2>&1
printf 'g\n' > "$REPO/g"; git -C "$REPO" add g
git -C "$REPO" commit -qm "sp-gcs-stalekey work" >/dev/null
tip1="$(git -C "$REPO" rev-parse "$BR2")"
base1="$(git -C "$REPO" rev-parse origin/main)"

slug2="$(printf '%s.%s' fixture "$BR2" | tr -c 'A-Za-z0-9._-' '_')"
D2="$SPIRA_RUN/gate-run/$slug2"
mkdir -p "$D2"
printf '0' > "$D2/rc"
printf '%s %s' "$tip1" "$base1" > "$D2/key"
date +%s > "$D2/started"
: > "$D2/out"

out1="$(gate-run.sh --status "$BR2" fixture 2>&1)"; rc1=$?
is "hand-built key matches gate-run.sh's own — recorded PASS answers 0" "0" "$rc1"

# A landing elsewhere moves the base, then this branch is rebased onto it — exactly the
# sequence aeon.sh's own post-close rebase performs on a branch that was left behind.
git -C "$REPO" checkout -q main
printf 'h\n' > "$REPO/h"; git -C "$REPO" add h
git -C "$REPO" commit -qm "unrelated landing" >/dev/null
git -C "$REPO" push -q origin main
git -C "$REPO" checkout -q "$BR2"
git -C "$REPO" rebase -q origin/main >/dev/null

out2="$(gate-run.sh --status "$BR2" fixture 2>&1)"; rc2=$?
is "rebased out from under a recorded PASS — status answers 4, not 3" "4" "$rc2"
want "st=4: message names a different tree, not silence" "different tree" "$out2"

# POSITIVE CONTROL — a branch that was truly never gated still answers 3, so 4 is not
# just 3 renamed everywhere.
BR3="spira/sp-gcs-nevergated"
git -C "$REPO" branch -q "$BR3" origin/main 2>/dev/null || true
out3="$(gate-run.sh --status "$BR3" fixture 2>&1)"; rc3=$?
is "never gated: status still answers 3" "3" "$rc3"

# ======================================================================================
echo
echo "gate-run.sh ITSELF (not the stub): a run that started and then died without ever"
echo "recording a verdict answers 5, never 1 — 1 must mean a real recorded FAIL, not a"
echo "corpse with no rc file (sp-k7klr):"
# ======================================================================================
BR4="spira/sp-gcs-died"
git -C "$REPO" branch -q "$BR4" origin/main 2>/dev/null || true
tip4="$(git -C "$REPO" rev-parse "$BR4")"
base4="$(git -C "$REPO" rev-parse origin/main)"

# A pid that is certainly dead by the time gate-run.sh reads it: fork and wait it out.
( : ) & deadpid4=$!; wait "$deadpid4" 2>/dev/null

slug4="$(printf '%s.%s' fixture "$BR4" | tr -c 'A-Za-z0-9._-' '_')"
D4="$SPIRA_RUN/gate-run/$slug4"
mkdir -p "$D4"
printf '%s' "$deadpid4" > "$D4/pid"
printf '%s %s' "$tip4" "$base4" > "$D4/key"
date +%s > "$D4/started"
: > "$D4/out"
# no rc file written — the run died before ever recording one

out4="$(gate-run.sh --status "$BR4" fixture 2>&1)"; rc4=$?
is   "died without a verdict answers 5, never 1" "5" "$rc4"
want "st=5: message says the run died without recording a verdict" "died" "$out4"
nowant "st=5: message must never claim FAILED" "FAILED" "$out4"

echo
tl_summary
