#!/usr/bin/env bash
#
# test-gate-preflight.sh — the gate's preflight checks fire with their evidence.
#
#   ./test-gate-preflight.sh
#
# THE DEFECT THIS PREVENTS. The base..branch diff suppressed git's stderr and then exited 1
# bare (sp-io5j), so an unresolvable branch produced a 13-byte log holding only the exit
# code. The gate now captures git's own diagnostic and includes it, so a missing branch or
# an unfetched base names itself rather than saying nothing.
#
# CASE 0 CLOSES THE HIGHEST-PRIORITY GAP IN docs/test-plan/gate-verdict.md (gap #1,
# UC-gate-verdict-03): an unreadable SPIRA_REPO_MAP used to PASS every branch — the gate
# saw no map, read that as "no gate command for anyone", and let the branch through a
# trial that never ran. Nothing exercised this path before now. It runs first among the
# fail-closed cases, per the bead's "fail-closed rows first" instruction.
#
# CASES 3-4 (SEEN RED / SEEN GREEN) ABSORB test-gate-missing-cmd.sh (sp-ajxg3, per
# docs/test-plan/gate-verdict.md section 4 cluster #2: "a passing gate run on a fresh
# fixture" was five suites each building their own remote-plus-clone to prove one
# positive control). gate.sh resolves every `bash <path>` in the repo-map gate command
# against the base tree before running any trial; a path absent from the base returns a
# distinct config-error verdict rather than BASE_FAIL. Naming BASE_FAIL for a file that
# does not exist yet sends diagnosis toward a failing suite that was never run — this
# blocked 22 branches with a misleading verdict (sp-lkzl).
#
# defect: sp-io5j sp-lkzl
# tier: T1
# covers: spira/gate.sh UC-gate-verdict-03 UC-gate-verdict-04 UC-gate-verdict-06 UC-gate-verdict-07
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testlib/gate-fixture.sh"

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
gate_fixture_init "$TMP"

# A real branch so the positive control can pass.
BR=spira/sp-pre1
gate_fixture_branch "$BR"

rungate() { gate_fixture_run "$1" repo "${@:2}"; }  # rungate <branch> [VAR=VAL ...]

echo "test-gate-preflight.sh — the gate's preflight checks fire with their evidence"

# --------------------------------------------------------------------------------------
# POSITIVE CONTROL. Before claiming the gate catches a bad diff, show it passes a good one.
# A suite that only checked "the gate refused" would pass against a gate that refused
# everything (law-absence-needs-a-positive-control).
# --------------------------------------------------------------------------------------
out="$(rungate "$BR")"; rc=$?
is   "a valid branch passes the gate"  0 "$rc"
want "and says PASS"                   "VERDICT=PASS" "$out"

# --------------------------------------------------------------------------------------
# CASE 0a — AN UNREADABLE REPO-MAP (gap #1, UC-gate-verdict-03). A mistyped path, an
# unmounted home, or a map a bad install deleted must never read as "no gate command" —
# it must refuse to judge at all. FAIL-CLOSED, RUN FIRST.
# --------------------------------------------------------------------------------------
out="$(rungate "$BR" SPIRA_REPO_MAP="$TMP/no-such-repo-map")"; rc=$?
is   "an unreadable repo-map exits NO_VERDICT, never PASS"    75 "$rc"
want "and names the reason no-repo-map-file"                  "reason=no-repo-map-file" "$out"
want "and the verdict line says NO_VERDICT"                   "VERDICT=NO_VERDICT" "$out"
nowant "and never says PASS"                                  "VERDICT=PASS" "$out"

# --------------------------------------------------------------------------------------
# CASE 0b — A REPO ABSENT FROM THE MAP (UC-gate-verdict-03's second half). The map IS
# readable; it simply names no entry for this repository.
# --------------------------------------------------------------------------------------
out="$(gate_fixture_run "$BR" no-such-repo-name)"; rc=$?
is   "a repo absent from the map exits NO_VERDICT"             75 "$rc"
want "and names the reason no-repo-map"                        "reason=no-repo-map" "$out"

# --------------------------------------------------------------------------------------
# CASE 1 — AN UNRESOLVABLE BRANCH. The base is valid (origin/main) but the branch does not
# exist. git diff says exactly what is wrong — "fatal: ambiguous argument ... unknown
# revision" — and the gate must include that rather than discarding it.
# --------------------------------------------------------------------------------------
out="$(rungate "spira/no-such-branch")"; rc=$?
is   "an unresolvable branch exits NO_VERDICT"       75 "$rc"
want "and names the reason as no-diff"                "reason=no-diff" "$out"
want "and the verdict line says NO_VERDICT"           "VERDICT=NO_VERDICT" "$out"
want "and the message names the base and branch"      "origin/main...spira/no-such-branch" "$out"
want "and the message names the repo"                 "repo" "$out"
want "and git's own diagnostic is included"           "fatal:" "$out"

# --------------------------------------------------------------------------------------
# CASE 2 — AN UNRESOLVABLE BASE. The branch exists but the base ref in the repo-map points
# at a ref that was never fetched. The diff fails for the same reason — one side does not
# resolve — and the message must say which.
# --------------------------------------------------------------------------------------
printf 'repo | %s | push | refs/remotes/origin/no-such-base |  | true\n' "$REPO" > "$MAP"
out="$(rungate "$BR")"; rc=$?
is   "an unresolvable base exits NO_VERDICT"          75 "$rc"
want "with reason no-diff or no-base"                 "NO_VERDICT" "$out"

# Restore the map for the missing-cmd cases below.
printf 'repo | %s | push | origin/main |  | true\n' "$REPO" > "$MAP"

# --------------------------------------------------------------------------------------
# CASE 3 — SEEN RED: THE GATE COMMAND NAMES A FILE ABSENT FROM THE BASE. The branch trial
# exits 127 (no such file) and so would a base trial run the same command, which is
# indistinguishable from BASE_FAIL unless the gate checks the base tree first.
# --------------------------------------------------------------------------------------
GATE_FILE="tools/check.sh"
printf 'repo | %s | push | origin/main |  | bash %s\n' "$REPO" "$GATE_FILE" > "$MAP"
gate_fixture_branch repo/sp-t1 missing-cmd.txt change
out="$(rungate repo/sp-t1)"; rc=$?
nowant "SEEN RED: gate does not pass" "VERDICT=PASS" "$out"
want   "SEEN RED: names the missing file" "$GATE_FILE" "$out"
nowant "SEEN RED: does not say BASE_FAIL" "BASE_FAIL" "$out"
nowant "SEEN RED: does not say base-red reason" "reason=base-red" "$out"

# --------------------------------------------------------------------------------------
# CASE 4 — SEEN GREEN (positive control for case 3): the same file, now present on the
# base. If the gate still refused after this, case 3 would be proving nothing.
# --------------------------------------------------------------------------------------
RUNS="$TMP/runs"; : > "$RUNS"
mkdir -p "$(dirname "$REPO/$GATE_FILE")"
printf '#!/usr/bin/env bash\nprintf "ran\\n" >> %s\necho ok\n' "$RUNS" > "$REPO/$GATE_FILE"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "add check.sh"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
gate_fixture_branch repo/sp-t2 missing-cmd2.txt change2
out="$(rungate repo/sp-t2)"; rc=$?
is     "SEEN GREEN: gate passes when file is present" 0 "$rc"
is     "SEEN GREEN: CMD was actually executed (ran file counted)" 1 "$(wc -l < "$RUNS" | tr -d ' ')"
nowant "SEEN GREEN: no config-error in output" "configuration error" "$out"

# --------------------------------------------------------------------------------------
# CASE 5 — SYNTAX-ONLY PASS (UC-gate-verdict-06). A repo-map row with an empty gate column
# has been fully judged by the universal layer (bash -n, beads-data, foreign-harness) — its
# trial IS syntax alone, and that is a PASS, not "no gate configured". The VERDICT line must
# still print: a caller must never have to tell "passed" from "exited early" (gate.sh L228).
# --------------------------------------------------------------------------------------
printf 'repo | %s | push | origin/main |  | \n' "$REPO" > "$MAP"
out="$(rungate "$BR")"; rc=$?
is   "an empty gate column exits PASS"          0 "$rc"
want "and names the reason syntax-only"         "reason=syntax-only" "$out"
want "and the verdict line still prints"        "VERDICT=PASS" "$out"

# Restore the map for any future cases.
printf 'repo | %s | push | origin/main |  | true\n' "$REPO" > "$MAP"
tl_summary
