#!/usr/bin/env bash
#
# test-release.sh — release.sh cut and show: a tag names exactly the beads
# landed since the previous tag, and show resolves it to ids and commits.
#
#   ./test-release.sh
#
# THE PROPERTY UNDER TEST
# -----------------------
# A release unit is an annotated git tag that records every bead id whose
# commit reached the base branch since the previous release tag. Four
# properties are checked:
#
#   1. FIRST TAG: with no prior release tag, cut walks the full base branch
#      history and creates a tag whose bead list matches the commit subjects.
#   2. SECOND TAG: with a prior tag, only commits SINCE that tag are included
#      — no more, no fewer (the core invariant).
#   3. ZERO-LANDED: when no new commits have landed since the previous tag,
#      cut produces no tag and says so.
#   4. SHOW: show resolves the tag to its bead ids and their commits.
#   5. BASE REF: the base ref comes from spira_landref, never assumed to be
#      'main' — the fixture uses 'trunk' so a hardcoded 'main' fails
#      immediately.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# --------------------------------------------------------
# The suite first verifies that at least one expected bead id appears in the
# tag before trusting any 'not present' assertions. It also verifies the
# suite itself fails when release.sh is absent.
#
# defect: sp-gsmx.3
# covers: spira/release.sh spira/lib.sh
# covers: spira/promote.sh
# scar: release.sh used the Spira id prefix as the base branch name instead of calling spira_landref; a repo with any non-prefix default branch silently cut tags against an empty history.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# Git identity for commits — required in an env-i run.
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# --------------------------------------------------------------------------------------
# GIT FIXTURE: a bare 'origin' with a 'trunk' default branch, and a clone.
# 'trunk' is deliberate — a hardcoded 'main' or 'master' in release.sh fails here.
# --------------------------------------------------------------------------------------
ORIGIN="$TMP/origin.git"
REPO="$TMP/repo"
git init -q --bare -b trunk "$ORIGIN"
git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t
git -C "$REPO" config user.name t

# Base commit — no bead id.
printf 'base\n' > "$REPO/f"
git -C "$REPO" add f
git -C "$REPO" commit -qm "initial commit"
git -C "$REPO" push -q origin trunk 2>/dev/null

# Three bead commits landing on trunk.
printf 'a\n' >> "$REPO/f"; git -C "$REPO" add f
git -C "$REPO" commit -qm "sp-aaa — first bead"

printf 'b\n' >> "$REPO/f"; git -C "$REPO" add f
git -C "$REPO" commit -qm "sp-bbb — second bead"

printf 'c\n' >> "$REPO/f"; git -C "$REPO" add f
git -C "$REPO" commit -qm "sp-ccc — third bead"

git -C "$REPO" push -q origin trunk 2>/dev/null

# --------------------------------------------------------------------------------------
# HARNESS FIXTURE: copy the scripts to a temp dir; point SPIRA_HOME there.
# repo-map maps 'fixture' to the temp git checkout, with 'trunk' as the base.
# The base column is set explicitly so spira_landref resolves without a network call.
# --------------------------------------------------------------------------------------
SH="$TMP/spira"
mkdir -p "$SH"
for f in release.sh unhold.sh lib.sh conf.sh suite-covers.sh; do
    [ -f "$HERE/$f" ] && cp "$HERE/$f" "$SH/"
done
chmod +x "$SH/release.sh"

REPO_MAP="$TMP/repo-map"
# Repo-map columns: name | path | land-mode | base | prefix | format
printf 'fixture | %s | push | origin/trunk | sp | |\n' "$REPO" > "$REPO_MAP"

run_cut() {   # run_cut [args] -> stdout
    env -i PATH="$PATH" HOME="$HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" \
        SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_REPO="$SH" \
        SPIRA_GOAL=sp-goal \
        SPIRA_ID_PREFIX=sp \
        bash "$SH/release.sh" cut "$@" 2>&1
}

run_show() {  # run_show <tag> -> stdout
    env -i PATH="$PATH" HOME="$HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" \
        SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_REPO="$SH" \
        SPIRA_GOAL=sp-goal \
        SPIRA_ID_PREFIX=sp \
        bash "$SH/release.sh" show "$@" 2>&1
}

mkdir -p "$TMP/run"

# ======================================================================================
echo
echo "1. POSITIVE CONTROL — first release tag includes all landed beads"
echo "   (verifies the matcher fires before trusting 'not present' checks)"
# ======================================================================================
tag1="$(run_cut fixture 2>&1)"
rc1=$?
is "cut exits 0"   0   "$rc1"
want "tag name has prefix"         "spira-release-fixture-" "$tag1"
want "sp-aaa in tag message"       "sp-aaa" "$(git -C "$REPO" tag -l --format='%(contents)' "$tag1" 2>/dev/null)"
want "sp-bbb in tag message"       "sp-bbb" "$(git -C "$REPO" tag -l --format='%(contents)' "$tag1" 2>/dev/null)"
want "sp-ccc in tag message"       "sp-ccc" "$(git -C "$REPO" tag -l --format='%(contents)' "$tag1" 2>/dev/null)"

# ======================================================================================
echo
echo "2. SECOND TAG — only beads landed after the first tag are included"
# ======================================================================================
# Add one more commit after the first release tag.
printf 'd\n' >> "$REPO/f"; git -C "$REPO" add f
git -C "$REPO" commit -qm "sp-ddd — fourth bead"
git -C "$REPO" push -q origin trunk 2>/dev/null

tag2="$(run_cut fixture 2>&1)"
rc2=$?
is "second cut exits 0"   0  "$rc2"
want   "sp-ddd is in second tag"        "sp-ddd" \
    "$(git -C "$REPO" tag -l --format='%(contents)' "$tag2" 2>/dev/null)"
nowant "sp-aaa is NOT in second tag"    "sp-aaa" \
    "$(git -C "$REPO" tag -l --format='%(contents)' "$tag2" 2>/dev/null)"
nowant "sp-bbb is NOT in second tag"    "sp-bbb" \
    "$(git -C "$REPO" tag -l --format='%(contents)' "$tag2" 2>/dev/null)"
nowant "sp-ccc is NOT in second tag"    "sp-ccc" \
    "$(git -C "$REPO" tag -l --format='%(contents)' "$tag2" 2>/dev/null)"

# Check prev: field names the first tag.
want "prev field names first tag" "prev: $tag1" \
    "$(git -C "$REPO" tag -l --format='%(contents)' "$tag2" 2>/dev/null)"

# ======================================================================================
echo
echo "3. ZERO-LANDED — no commits since previous tag; no tag is created"
# ======================================================================================
existing_count="$(git -C "$REPO" tag -l 'spira-release-fixture-*' | wc -l | tr -d ' ')"
out_empty="$(run_cut fixture 2>&1)"
rc_empty=$?
new_count="$(git -C "$REPO" tag -l 'spira-release-fixture-*' | wc -l | tr -d ' ')"
is "cut exits 0 with nothing to tag"  0   "$rc_empty"
is "no new tag created"               "$existing_count" "$new_count"
want "says nothing to tag" "no beads landed" "$out_empty"

# ======================================================================================
echo
echo "4. SHOW — resolves tag to bead ids and commits"
# ======================================================================================
show_out="$(run_show "$tag1" 2>&1)"
rc_show=$?
is "show exits 0"  0  "$rc_show"
want "bead: sp-aaa in show output"   "bead: sp-aaa"  "$show_out"
want "bead: sp-bbb in show output"   "bead: sp-bbb"  "$show_out"
want "bead: sp-ccc in show output"   "bead: sp-ccc"  "$show_out"
want "sp-aaa commit shown"           "sp-aaa"        "$show_out"
want "commits section present"       "commits:"      "$show_out"

# ======================================================================================
echo
echo "5. BASE REF — fixture uses 'trunk'; a hardcoded 'main' would fail"
# ======================================================================================
# If release.sh had hardcoded 'main', cut would fail to resolve the base. The
# successful cut in case 1 already proves this, but we make the dependency explicit.
want "base line names origin/trunk" "base: origin/trunk" \
    "$(git -C "$REPO" tag -l --format='%(contents)' "$tag1" 2>/dev/null)"

# ======================================================================================
echo
echo "6. SUITE SELF-CHECK — suite fails without release.sh"
# ======================================================================================
rm -f "$SH/release.sh"
out_absent="$(run_cut fixture 2>&1 || true)"
nowant "tag prefix absent without script" "spira-release-fixture-" "$out_absent"

# ======================================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
