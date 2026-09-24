#!/usr/bin/env bash
#
# test-gate-fixture-diag.sh — gate's error output includes fixture-build diagnostic,
# not just the generic "could not build" message.
#
# THE DEFECT THIS PREVENTS. A gate suite that fails to build its fixture emits two
# pieces of output: the actual reason from the builder (written to stderr), and then
# the generic "could not build a fixture database" echo. When the gate ran further
# suites after the failure, their one-line "ok" entries pushed the builder diagnostic
# past the output window gate.sh applied with `| tail -20`, leaving only the later
# lines. The operator read the gate's error message and saw passing suites, not the
# fixture build failure that caused the red. The fix removes the truncation: the gate
# command's full output passes through, and the diagnostic is present wherever in the
# run it occurs.
#
# THE POSITIVE CONTROL IS ESSENTIAL. A check that reports "diagnostic present" without
# first confirming the diagnostic COULD have been absent proves nothing. Case 2 below
# verifies that the offender (the diagnostic token) is correctly identified, then Case
# 1 verifies it survives a run where tail -20 would have lost it.
#
# tier: T1
# covers: spira/gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
gone(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] absent in [$3]"; }

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
MAP="$TMP/repo-map"; VDIR="$TMP/verdicts"; GATELOG="$TMP/gate.log"; HOMEDIR="$TMP/home"
mkdir -p "$RUN/worktree" "$HOMEDIR" "$SH"

# The gate under test is a copy — the harness's own bytes are part of the verdict key,
# so the installed copy must not be what is tested.
cp "$HERE/gate.sh" "$HERE/gate-lib.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/exclude.sh" "$HERE/skew.sh" \
   "$HERE/yield.sh" "$HERE/suite-covers.sh" "$SH/"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
printf 'base\n' > "$REPO/marker"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin

w="$TMP/work"
git -C "$REPO" worktree add -q -b "spira/sp-t1" "$w" origin/main
printf 'change\n' > "$w/change.txt"
git -C "$w" add -A; git -C "$w" commit -q -m "feat: sp-t1 — work"
git -C "$REPO" worktree remove --force "$w"

rungate() {
    local br="$1"; shift
    env -i HOME="$HOMEDIR" PATH="/usr/bin:/bin" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_REPO_MAP="$MAP" SPIRA_GATE_LOG="$GATELOG" \
        SPIRA_VERDICTS="$VDIR" SPIRA_VERDICT_TTL=0 \
        "$@" bash "$SH/gate.sh" "$br" repo 2>&1
}

# A stable token embedded in the fixture-builder diagnostic. If it appears in the
# gate output, the builder's stderr was preserved; if absent, it was suppressed.
DIAG_TOKEN="FIXTURE_BUILD_DIAG_SURVIVED"

# Write the gate command to a script file so the gate-command string in the repo-map
# stays a single line (awk processes one line at a time; a newline inside the gate
# column would truncate the command at the first line break).
GATE_SCRIPT="$TMP/gate-cmd.sh"
cat > "$GATE_SCRIPT" << EOF
#!/bin/bash
# base (SPIRA_GATE_BRANCH = SPIRA_GATE_BASE): exit 0 — base is healthy.
# branch: simulate a suite that (1) emits fixture-build diagnostic to stderr,
#         (2) emits the generic "could not build" message to stdout, (3) produces
#         25 subsequent "ok" lines to stderr — enough to push the diagnostic
#         past a tail -20 window — then exits 1.
[ "\$SPIRA_GATE_BRANCH" = "\$SPIRA_GATE_BASE" ] && exit 0
printf 'testdb: bd init failed (rc=1) for sptest_t1 in /tmp/sptest_t1\ntestdb:   Error: ${DIAG_TOKEN}\n' >&2
echo 'test-suite.sh: could not build a fixture database'
for _i in \$(seq 1 25); do printf 'gate: suite-%s.sh ok   cost=1s\n' "\$_i" >&2; done
printf 'gate: cost total=25s budget=300s\n' >&2
exit 1
EOF
chmod +x "$GATE_SCRIPT"
printf 'repo | %s | push | origin/main |  | bash %s\n' "$REPO" "$GATE_SCRIPT" > "$MAP"

echo "test-gate-fixture-diag.sh — fixture-build diagnostic survives in gate output"

# --------------------------------------------------------------------------------------
# CASE 2 — POSITIVE CONTROL. Before asserting the diagnostic survives in the gate
# output, confirm the gate command actually emits it. An empty or misquoted script
# would otherwise produce a silent all-clear on the real assertion.
# --------------------------------------------------------------------------------------
ctrl_out="$(rungate "spira/sp-t1" 2>&1)"; ctrl_rc=$?
is   "positive control: gate exits 1 (branch is red)"            1             "$ctrl_rc"
want "positive control: diagnostic token is in gate output"       "$DIAG_TOKEN" "$ctrl_out"

# --------------------------------------------------------------------------------------
# CASE 1 — THE ASSERTION. The diagnostic appears in the gate's error output even
# though 25 "ok" lines follow it. A tail -20 inside run_gate() would have dropped
# lines 1–9, losing the diagnostic on lines 1–2 (the token is on line 2).
# --------------------------------------------------------------------------------------
out="$(rungate "spira/sp-t1" 2>&1)"; gate_rc=$?
is   "gate exits 1 on a red branch"                              1             "$gate_rc"
want "builder diagnostic survives in gate output"                "$DIAG_TOKEN" "$out"
want "generic failure message also present"                      "could not build a fixture database" "$out"

# --------------------------------------------------------------------------------------
# CASE 3 — A PASSING GATE CARRIES NO SPURIOUS DIAGNOSTIC. The fix must not leak the
# diagnostic token into a gate run on a branch whose gate command exits 0.
# --------------------------------------------------------------------------------------
printf 'repo | %s | push | origin/main |  | exit 0\n' "$REPO" > "$MAP"
pass_out="$(rungate "spira/sp-t1" 2>&1)"
gone "diagnostic absent from a passing gate" "$DIAG_TOKEN" "$pass_out"
tl_summary
