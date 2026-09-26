#!/usr/bin/env bash
#
# test-gate-universal.sh — the universal layer runs through gate.sh itself, on every
# repository regardless of its own gate command.
#
#   ./test-gate-universal.sh
#
# docs/test-plan/gate-verdict.md gap #3 (UC-gate-verdict-05). test-skew-foreign.sh already
# proves skew.sh's own exit codes in isolation, but nothing before this ran a changed *.sh
# file, vendored beads data or a vendored harness copy THROUGH gate.sh and checked the
# VERDICT line it produces — or that a missing exclude.sh/skew.sh, or skew exiting 3, is
# NO_VERDICT rather than a silent pass or a branch-blaming FAIL
# (class: sp-gate-conf-fail-as-foreign-harness).
#
# tier: T2
# covers: spira/gate.sh spira/exclude.sh spira/skew.sh UC-gate-verdict-05
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testlib/gate-fixture.sh"

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
gate_fixture_init "$TMP"

rungate() { gate_fixture_run "$1" repo "${@:2}"; }  # rungate <branch> [VAR=VAL ...]

echo "test-gate-universal.sh — the universal layer (UC-gate-verdict-05)"

# --------------------------------------------------------------------------------------
# POSITIVE CONTROL. An ordinary branch touches none of the universal layer's fences and
# passes — required before any refusal below means the layer is discriminating rather than
# refusing everything (law-absence-needs-a-positive-control).
# --------------------------------------------------------------------------------------
gate_fixture_branch spira/sp-u0 ok.txt fine
out="$(rungate spira/sp-u0)"; rc=$?
is "positive control: an ordinary branch passes" 0 "$rc"

# --------------------------------------------------------------------------------------
# SYNTAX. A changed *.sh file that fails bash -n is FAIL, never BASE_FAIL or NO_VERDICT —
# this is the one universal check that runs before the other two even set up their guards.
# --------------------------------------------------------------------------------------
gate_fixture_branch spira/sp-u1 broken.sh 'if true; then'
out="$(rungate spira/sp-u1)"; rc=$?
is   "syntax: a broken *.sh exits FAIL"   1 "$rc"
want "syntax: names the reason"           "reason=syntax" "$out"
want "syntax: names the offending file"   "broken.sh" "$out"

# --------------------------------------------------------------------------------------
# BEADS-DATA. A branch that would land beads data anywhere in its tree is FAIL. The branch
# also carries the harness signature (boundary/gate.sh/lib.sh) deliberately — exclude.sh's
# filter needs it to know its own scope, and without it the check has nothing to scan and
# says so rather than passing.
# --------------------------------------------------------------------------------------
w="$(mktemp -d)"
git -C "$REPO" worktree add -q -b spira/sp-u2 "$w" origin/main
: > "$w/boundary"; : > "$w/gate.sh"; : > "$w/lib.sh"
printf 'x\n' > "$w/notes.jsonl"
git -C "$w" add -A; git -C "$w" commit -q -m "vendor beads data"
git -C "$REPO" worktree remove --force "$w"
out="$(rungate spira/sp-u2)"; rc=$?
is   "beads-data: a branch landing beads data exits FAIL" 1 "$rc"
want "beads-data: names the reason"                       "reason=beads-data" "$out"
want "beads-data: names the offending file"               "notes.jsonl" "$out"

# --------------------------------------------------------------------------------------
# FOREIGN-HARNESS. A repository that is not the harness's own may not change a vendored
# copy of it. SPIRA_REPO is overridden to a THIRD, unrelated repository so the fixture repo
# stops being its own exemption — gate_fixture_run otherwise sets SPIRA_REPO="$REPO",
# which makes every fixture repo its own "home" and this fence untestable through it.
# --------------------------------------------------------------------------------------
OTHERHOME="$TMP/other-home"
git init -q -b main "$OTHERHOME"
printf 'x\n' > "$OTHERHOME/f"; git -C "$OTHERHOME" add -A; git -C "$OTHERHOME" commit -q -m base

w="$(mktemp -d)"
git -C "$REPO" worktree add -q -b spira/sp-u3 "$w" origin/main
mkdir -p "$w/.tools/spira"
: > "$w/.tools/spira/boundary"; printf 'x\n' > "$w/.tools/spira/gate.sh"; : > "$w/.tools/spira/lib.sh"
git -C "$w" add -A; git -C "$w" commit -q -m "vendor a harness copy"
printf 'changed\n' >> "$w/.tools/spira/gate.sh"
git -C "$w" add -A; git -C "$w" commit -q -m "touch the vendored copy"
git -C "$REPO" worktree remove --force "$w"
out="$(rungate spira/sp-u3 SPIRA_REPO="$OTHERHOME")"; rc=$?
is   "foreign-harness: a branch touching a vendored copy exits FAIL" 1 "$rc"
want "foreign-harness: names the reason"                             "reason=foreign-harness" "$out"
want "foreign-harness: names the vendored path"                      ".tools/spira/gate.sh" "$out"

# --------------------------------------------------------------------------------------
# MISSING-EXCLUDE. exclude.sh absent from this copy of the harness is a machinery fault —
# the beads-data fence has nothing to run and must say so, never silently skip the check.
# --------------------------------------------------------------------------------------
gate_fixture_branch spira/sp-u4 ok4.txt fine
mv "$SH/exclude.sh" "$TMP/exclude.sh.aside"
out="$(rungate spira/sp-u4)"; rc=$?
is   "missing-exclude: exits NO_VERDICT" 75 "$rc"
want "missing-exclude: names the reason" "reason=missing-exclude" "$out"
mv "$TMP/exclude.sh.aside" "$SH/exclude.sh"

# --------------------------------------------------------------------------------------
# MISSING-SKEW. Same shape, for skew.sh.
# --------------------------------------------------------------------------------------
gate_fixture_branch spira/sp-u5 ok5.txt fine
mv "$SH/skew.sh" "$TMP/skew.sh.aside"
out="$(rungate spira/sp-u5)"; rc=$?
is   "missing-skew: exits NO_VERDICT" 75 "$rc"
want "missing-skew: names the reason" "reason=missing-skew" "$out"

# --------------------------------------------------------------------------------------
# SKEW-INIT-FAULT. skew.sh exiting 3 (its own "could not check", e.g. a database lock mid
# bd-migrate) is a machinery fault, never read as a foreign-harness violation
# (class: sp-gate-conf-fail-as-foreign-harness) — the branch is not at fault for the box's
# database being unavailable.
# --------------------------------------------------------------------------------------
printf '#!/usr/bin/env bash\nexit 3\n' > "$SH/skew.sh"
gate_fixture_branch spira/sp-u6 ok6.txt fine
out="$(rungate spira/sp-u6)"; rc=$?
is     "skew-init-fault: exits NO_VERDICT, not FAIL" 75 "$rc"
want   "skew-init-fault: names the reason"           "reason=skew-init-fault" "$out"
nowant "skew-init-fault: is never charged as a FAIL" "VERDICT=FAIL" "$out"
nowant "skew-init-fault: never uses the foreign-harness reason slug" "reason=foreign-harness" "$out"

tl_summary
