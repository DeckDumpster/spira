#!/usr/bin/env bash
#
# test-id-prefix.sh — SPIRA_ID_PREFIX is honoured at every detection site.
#
# other_beads_on_conflicts in lib.sh previously hardcoded "sp-" (grep pattern)
# and silently returned "nothing found" for any installation with a
# non-default prefix.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): prove the
# detector fires before trusting the negative case.
#
# covers: spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1"; esac; }
wantrc() { if [ "$3" -eq "$2" ]; then ok "$1"; else bad "$1" "wanted rc=$2, got rc=$3"; fi; }

echo "test-id-prefix.sh"

PREFIX=tt

# ============================================================================
echo
echo "other_beads_on_conflicts — non-default prefix"
# ============================================================================

TMP_REPO="$(mktemp -d)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_REPO" "$TMP_DIR"' EXIT INT TERM

git -C "$TMP_REPO" init -q
git -C "$TMP_REPO" config user.email "test@example.com"
git -C "$TMP_REPO" config user.name "Test"

echo "a" > "$TMP_REPO/f.txt"
git -C "$TMP_REPO" add f.txt
git -C "$TMP_REPO" commit -q -m "initial"

# Commit on base touching f.txt with a custom-prefix bead id in the subject.
echo "b" > "$TMP_REPO/f.txt"
git -C "$TMP_REPO" add f.txt
git -C "$TMP_REPO" commit -q -m "tt-abc123: fix the thing"
BASE="$(git -C "$TMP_REPO" rev-parse HEAD)"

# Branch from before that commit — represents the aeon's working branch.
git -C "$TMP_REPO" checkout -q -b "spira/tt-own" HEAD~1
echo "c" > "$TMP_REPO/f.txt"
git -C "$TMP_REPO" add f.txt
git -C "$TMP_REPO" commit -q -m "tt-own: branch work"

# POSITIVE CONTROL: with SPIRA_ID_PREFIX=tt, tt-abc123 must be found.
result="$(
    export SPIRA_ID_PREFIX="$PREFIX" SPIRA_HOME="$HERE" SPIRA_RUN="$TMP_DIR/run"
    export SPIRA_DB="$TMP_DIR/db"
    bash -c '. "$1/lib.sh"; other_beads_on_conflicts "$2" "spira/tt-own" "$3" "f.txt"' \
        -- "$HERE" "$TMP_REPO" "$BASE"
)"
want   "custom-prefix id found when branch prefix matches"          "tt-abc123" "$result"
nowant "own bead excluded from conflict result"                     "tt-own"    "$result"

# NEGATIVE CONTROL: a branch with sp- prefix must not match tt- ids on the base.
# The prefix is derived from the branch's own id, not SPIRA_ID_PREFIX.
result_sp="$(
    export SPIRA_HOME="$HERE" SPIRA_RUN="$TMP_DIR/run"
    export SPIRA_DB="$TMP_DIR/db"
    bash -c '. "$1/lib.sh"; other_beads_on_conflicts "$2" "spira/sp-own" "$3" "f.txt"' \
        -- "$HERE" "$TMP_REPO" "$BASE"
)"
nowant "sp-prefix branch does not match tt- ids on base"            "tt-abc123" "$result_sp"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
