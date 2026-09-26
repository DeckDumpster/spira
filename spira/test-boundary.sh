#!/usr/bin/env bash
#
# test-boundary.sh — the filesystem check that boundary.sh check now runs.
#
#   ./test-boundary.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# boundary.sh check compared the rendered manifest against the published targets but never
# verified that the manifest rows themselves referenced existing files. A deletion that left
# its row standing passed the staleness check undetected because the rendered output still
# matched the published table — both sides agreed on a file that had not existed for three
# commits. This suite pins the new behavior: a row naming a nonexistent path must fail
# `check`, and a clean manifest must pass.
#
# THE POSITIVE CONTROL COMES FIRST (law-absence-needs-a-positive-control). A check that
# cannot detect anything and a check that happens to find a clean tree print the same thing.
# The plant comes before the clean assertion, so the clean reading means something.
#
# THE FIXTURE IS A MINIMAL SCRATCH HARNESS rooted under TMP. boundary.sh derives the
# repository root one directory up from itself, so the scratch layout mirrors the shipped
# layout: TMP/spira/boundary.sh, TMP/spira/boundary (the manifest), TMP/README.md (the
# published target). conf.sh is stubbed to set SPIRA_WIKI="" — this test is for the
# filesystem check, not for wiki rendering.
#
# tier: T1
# covers: spira/boundary.sh spira/boundary
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-boundary.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------------------
# THE SCRATCH HARNESS. boundary.sh resolves README as "$HERE/../README.md" and sources
# conf.sh from the same directory; the wiki target is skipped when SPIRA_WIKI is empty.
# ---------------------------------------------------------------------------------------
HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"

# Stub conf.sh: skip the wiki check so the test is not coupled to the operator's wiki page.
printf 'SPIRA_WIKI=""\n' > "$HARNESS/spira/conf.sh"

# A README with the required marker pair.
printf '# README\n<!-- BOUNDARY:BEGIN -->\nstale\n<!-- BOUNDARY:END -->\n' \
    > "$HARNESS/README.md"

# Copy the real boundary.sh into the scratch harness so it resolves paths relative to itself.
cp "$HERE/boundary.sh" "$HARNESS/spira/boundary.sh"

run_check() {   # run_check <manifest-content> -> exit status; stderr on stdout
    printf '%s\n' "$1" > "$HARNESS/spira/boundary"
    bash "$HARNESS/spira/boundary.sh" check 2>&1; echo "__rc=$?"
}

# ---------------------------------------------------------------------------------------
# THE POSITIVE CONTROL: a manifest row naming a nonexistent spira path must fail check.
# ---------------------------------------------------------------------------------------
out="$(run_check 'spira | spira/does-not-exist.sh | something')"
rc="${out##*__rc=}"; out="${out%__rc=*}"
is   "SEEN RED: nonexistent path fails check"    "1" "$rc"
want "and names the missing path"                "does-not-exist.sh" "$out"

# ---------------------------------------------------------------------------------------
# PROSE IN THE PATH FIELD is not a filesystem path and must not be checked.
# boundary.sh render distinguishes prose from paths by the presence of a space.
# ---------------------------------------------------------------------------------------
out="$(run_check 'brain | a tasks generator | something prose here')"
rc="${out##*__rc=}"; out="${out%__rc=*}"
# rc may be 1 because README is stale, but NOT because of a filesystem miss on the prose
nowant "prose in a brain row is not checked against the filesystem" "does-not-exist" "$out"
nowant "and no 'does not exist' error appears for it"               "does not exist"  "$out"

# ---------------------------------------------------------------------------------------
# A REAL PATH THAT EXISTS: a row pointing at a file that is present must pass fscheck.
# The README will still be stale (we planted "stale" as its body), so check exits 1 for
# the staleness reason — but not for a filesystem miss.
# ---------------------------------------------------------------------------------------
out="$(run_check "spira | spira/boundary.sh | the renderer")"
rc="${out##*__rc=}"; out="${out%__rc=*}"
nowant "existing path does not trigger a filesystem error" "does not exist" "$out"
want   "but check still fails because README is stale"    "stale"          "$out"

# ---------------------------------------------------------------------------------------
# AFTER write: once the target matches the manifest the staleness error also clears.
# ---------------------------------------------------------------------------------------
printf 'spira | spira/boundary.sh | the renderer\n' > "$HARNESS/spira/boundary"
bash "$HARNESS/spira/boundary.sh" write >/dev/null 2>&1
out="$(bash "$HARNESS/spira/boundary.sh" check 2>&1)"; rc=$?
is   "GREEN: existing path + fresh README passes check" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"

# ---------------------------------------------------------------------------------------
# THE SHIPPED TREE passes check with SPIRA_WIKI unset so only the README is tested.
# This is the assertion that the stale rows are gone and the renderer is current.
# ---------------------------------------------------------------------------------------
out="$(SPIRA_WIKI="" bash "$HERE/boundary.sh" check 2>&1)"; rc=$?
is   "the shipped tree passes check" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"
tl_summary
