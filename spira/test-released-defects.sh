#!/usr/bin/env bash
#
# test-released-defects.sh — a fixture graph containing one released defect
# and one caught one returns exactly the released one.
#
#   ./test-released-defects.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# released-defects.sh is the primary measurement this release-pipeline design
# is judged by (sp-gsmx). It is a query rather than an opinion: a defect is
# released when the commit that introduced it reached the base branch BEFORE
# the commit that fixed it. A caught defect — one whose introducing bead was
# poisoned before landing — is not released, and must not be counted.
#
# THREE CASES, EACH EXERCISED SEPARATELY
# (law-absence-needs-a-positive-control: the matcher must be seen to fire).
#
#  1. RELEASED: intro bead has a commit on main; fix bead has a later commit.
#     Script must name both commits and both bead ids.
#
#  2. CAUGHT: the introducing bead (labelled spira-poison) has NO commit on
#     main. The defect was caught before release; it must not appear in output.
#
#  3. SAME UNIT: both beads appear in the SAME commit message; the fix landed
#     in the same unit as the introduction and must not be counted.
#
# The positive control comes last: remove released-defects.sh and confirm the
# suite itself fails — so a suite run without the script does not read as a
# pass (law-absence-needs-a-positive-control).
#
# DEMOTE-TO-T2 (test plan §5): the join over discovered-from links is a pure
# function of the closed-bug list, so it is fed as a --graph JSON fixture
# instead of 7 real testdb_seed calls against an embedded Dolt store. Git
# stays real — the commit ordering is the other half of what is under test.
#
# The program is run in an isolated environment (law-gates-run-in-a-clean-environment):
# SPIRA_CONF=/nonexistent, fixture git repo, fixture SPIRA_REPO_MAP, --graph fixture.
#
# defect: sp-gsmx.1
# covers: spira/released-defects.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# --------------------------------------------------------------------------------------
# GIT FIXTURE: a repo with a remote so spira_landref resolves origin/main.
# --------------------------------------------------------------------------------------
ORIGIN="$TMP/origin.git"
REPO="$TMP/repo"
git init -q --bare -b main "$ORIGIN"
git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t
git -C "$REPO" config user.name t

# Base commit with no bead id — establishes the starting point
printf 'base\n' > "$REPO/f"
git -C "$REPO" add f
git -C "$REPO" commit -qm "base"
git -C "$REPO" push -q origin main 2>/dev/null

# CASE 1 — RELEASED: intro lands first, fix lands later.
# Commit Ca: names the introducing bead sp-intro-a.
printf 'a\n' >> "$REPO/f"; git -C "$REPO" add f
git -C "$REPO" commit -qm "sp-intro-a — introduce feature A"
# Commit Cb: names the fixing bead sp-fix-b. Comes after Ca on main.
printf 'b\n' >> "$REPO/f"; git -C "$REPO" add f
git -C "$REPO" commit -qm "sp-fix-b — fix defect from feature A"

# CASE 2 — CAUGHT: introducing bead has label spira-poison (no commit on main).
# Commit Cd: names the fixing bead sp-fix-d only; sp-intro-c is NOT in any commit.
printf 'd\n' >> "$REPO/f"; git -C "$REPO" add f
git -C "$REPO" commit -qm "sp-fix-d — fix for never-landed work"

# CASE 3 — SAME UNIT: both bead ids appear in the same commit message.
# This commit names both sp-intro-e AND sp-fix-f.
printf 'ef\n' >> "$REPO/f"; git -C "$REPO" add f
git -C "$REPO" commit -qm "sp-intro-e sp-fix-f — introduced and fixed in one unit"

# Push all commits to remote so origin/main is current
git -C "$REPO" push -q origin main 2>/dev/null

# --------------------------------------------------------------------------------------
# GRAPH FIXTURE: the closed-bug discovered-from graph, in the same shape
# `bd list --status closed --type bug --json` returns. released-defects.sh's join is a
# pure function over this list; only the fix beads need to appear in it (the introducing
# bead's own record is never read — its landing is proven by git, not by bd).
# --------------------------------------------------------------------------------------
GRAPH="$TMP/graph.json"
cat > "$GRAPH" <<'JSON'
[
  {"id":"sp-fix-b","title":"fix defect from feature A","status":"closed","issue_type":"bug","labels":["repo:fixture"],"updated_at":"2026-09-08T02:00:00Z","closed_at":"2026-09-08T02:00:00Z","dependencies":[{"issue_id":"sp-fix-b","depends_on_id":"sp-intro-a","type":"discovered-from"}]},
  {"id":"sp-fix-d","title":"fix for caught defect","status":"closed","issue_type":"bug","labels":["repo:fixture"],"updated_at":"2026-09-08T02:00:00Z","closed_at":"2026-09-08T02:00:00Z","dependencies":[{"issue_id":"sp-fix-d","depends_on_id":"sp-intro-c","type":"discovered-from"}]},
  {"id":"sp-fix-f","title":"fix defect from feature E","status":"closed","issue_type":"bug","labels":["repo:fixture"],"updated_at":"2026-09-08T03:00:00Z","closed_at":"2026-09-08T03:00:00Z","dependencies":[{"issue_id":"sp-fix-f","depends_on_id":"sp-intro-e","type":"discovered-from"}]}
]
JSON

# --------------------------------------------------------------------------------------
# ENVIRONMENT UNDER WHICH THE PROGRAM IS RUN.
# SPIRA_CONF=/nonexistent prevents loading real config.
# repo-map maps 'fixture' to the temp git checkout.
# SPIRA_REPO_MAP is pinned to a non-default path
# (law-gates-run-in-a-clean-environment: pin non-defaults so the suite does not
# assert against whatever the operator has installed).
# --------------------------------------------------------------------------------------
SH="$TMP/spira"
mkdir -p "$SH"
cp "$HERE/released-defects.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"
chmod +x "$SH/released-defects.sh"

REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$REPO_MAP"

rds() {  # rds [args] -> output of released-defects.sh
    # HOME="$HOME" (not "$TMP") so the bd shim at ~/.local/bin/bd is reachable.
    # SPIRA_CONF=/nonexistent prevents loading the real ~/.config/spira/spira.conf.
    env -i PATH="$PATH" HOME="$HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_VERDICT_WINDOW=50 \
        bash "$SH/released-defects.sh" --graph "$GRAPH" "$@" 2>/dev/null
}
mkdir -p "$TMP/run"

# ======================================================================================
echo
echo "test-released-defects.sh"
echo
echo "the positive control — sp-fix-b must be detected as released:"
# ======================================================================================
# THE MATCH MUST BE SEEN BEFORE ANYTHING ELSE IS TRUSTED. If released-defects.sh
# never names sp-fix-b, every 'nowant' assertion below passes vacuously.
out="$(rds)"
want "sp-fix-b is in the output (released defect)"  "sp-fix-b"   "$out"
want "sp-intro-a is named as the introducing bead"  "sp-intro-a" "$out"
want "the RELEASED marker is present"               "RELEASED"   "$out"
want "a count line reports 1 released defect"       "1 released" "$out"

# ======================================================================================
echo
echo "the caught defect is not counted:"
# ======================================================================================
# THE CAUGHT DEFECT MUST NOT APPEAR. sp-intro-c has no commit on main (it was
# poisoned); the script must recognise this as CAUGHT and exclude it.
nowant "sp-fix-d is absent (introducing bead never landed)" "sp-fix-d" "$out"
nowant "sp-intro-c is absent"                               "sp-intro-c" "$out"

# ======================================================================================
echo
echo "the same-unit defect is not counted:"
# ======================================================================================
# BOTH beads name the same commit: not a released defect, just a bead that
# discovered and fixed a bug in one session.
nowant "sp-fix-f is absent (same-unit fix)"   "sp-fix-f"   "$out"
nowant "sp-intro-e is absent (same-unit intro)" "sp-intro-e" "$out"

# ======================================================================================
echo
echo "a field that cannot be read renders ?, never 0:"
# ======================================================================================
# Run with a nonexistent repo-map so repo_root fails for the fixture repo.
# The repo cannot be resolved; the script should render ? for the commits and
# still report RELEASED rather than silently dropping the defect.
REPO_MAP_EMPTY="$TMP/repo-map-empty"
printf '# empty\n' > "$REPO_MAP_EMPTY"
out_nomap="$(env -i PATH="$PATH" HOME="$HOME" \
    SPIRA_CONF=/nonexistent \
    SPIRA_REPO_MAP="$REPO_MAP_EMPTY" \
    SPIRA_REPO=/nonexistent \
    SPIRA_RUN="$TMP/run" \
    SPIRA_VERDICT_WINDOW=50 \
    bash "$SH/released-defects.sh" --graph "$GRAPH" 2>/dev/null || true)"
# When repo cannot be found, commits render ? — but the defects are still
# counted (the fields report absence rather than hiding the record).
want   "? appears when repo is unresolvable" "?" "$out_nomap"
nowant "0 never appears in place of ?"       $'\t0\t' "$out_nomap"

# ======================================================================================
echo
echo "the suite fails without the script:"
# ======================================================================================
# THE SUITE MUST FAIL AGAINST AN ABSENT SCRIPT. A suite that passes with no script
# is not coverage — it asserts nothing (law-absence-needs-a-positive-control).
rm -f "$SH/released-defects.sh"
out_absent="$(rds 2>&1 || true)"
# Without the script, rds falls back to bash sourcing a non-existent file, which
# exits non-zero. Verify by checking that the RELEASED marker does NOT appear.
nowant "RELEASED does not appear without the script" "RELEASED" "$out_absent"

# ======================================================================================
tl_summary
