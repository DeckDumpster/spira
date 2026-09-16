#!/usr/bin/env bash
#
# test-skew-check-release.sh — skew.sh check asks whether the activated release is the
# latest published and whether its MANIFEST commit matches its release tag.
#
# WHAT THIS SUITE IS FOR
# ----------------------
# After the release-unit epic, the installed Spira is read-only: the only way to change
# it is to activate a new release tarball. A drift check against a tree that cannot drift
# reports clean forever, indistinguishable from a broken check. The new question is:
#   • Is the ACTIVATED release the latest published one?
#   • Does its MANIFEST commit match its release tag?
#
# THE POSITIVE CONTROLS ARE FIRST (law-absence-needs-a-positive-control):
#   1. An activated release that is NOT the latest → must report NOT-LATEST.
#   2. A MANIFEST whose commit does not match its tag → must report MANIFEST-MISMATCH.
# Both are seen RED (on old code) before the fix, then GREEN after.
#
# THE FIXTURE IS A REAL GIT REPO with real annotated tags so the check can resolve
# commits from release tags. Timestamps are fixed strings for determinism.
#
# covers: spira/skew.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-skew-check-release.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# SPIRA_REPO: a git checkout with two commits and two release tags.
# The tags follow the spira-release-<name>-<timestamp> convention from release.sh.
# ---------------------------------------------------------------------------
REPO="$TMP/repo"
git init -q "$REPO"
git -C "$REPO" config user.email "test@test"
git -C "$REPO" config user.name "test"
mkdir -p "$REPO/spira"
printf '# boundary\n'        > "$REPO/spira/boundary"
printf '#!/usr/bin/env bash\n' > "$REPO/spira/gate.sh"
printf '#!/usr/bin/env bash\n' > "$REPO/spira/lib.sh"
git -C "$REPO" add spira/
git -C "$REPO" commit -q -m "base"
COMMIT1="$(git -C "$REPO" rev-parse HEAD)"

printf '# v2\n' >> "$REPO/spira/lib.sh"
git -C "$REPO" add spira/lib.sh
git -C "$REPO" commit -q -m "advance"
COMMIT2="$(git -C "$REPO" rev-parse HEAD)"

TS1="20260912T100000Z"
TS2="20260912T120000Z"
git -C "$REPO" tag -a "spira-release-spira-${TS1}" "$COMMIT1" \
    -m "$(printf 'spira release: spira\nbase: main (%s)\nprev: (none)\n\nbead: sp-test1' "$COMMIT1")"
git -C "$REPO" tag -a "spira-release-spira-${TS2}" "$COMMIT2" \
    -m "$(printf 'spira release: spira\nbase: main (%s)\nprev: spira-release-spira-%s\n\nbead: sp-test2' "$COMMIT2" "$TS1")"

# ---------------------------------------------------------------------------
# Releases directory: two unpacked release directories, current symlink.
# ---------------------------------------------------------------------------
RELEASES="$TMP/releases"
mkdir -p "$RELEASES"

REL1_DIR="$RELEASES/spira-${TS1}"
mkdir -p "$REL1_DIR/spira"
printf 'commit %s\ntimestamp %s\n' "$COMMIT1" "$TS1" > "$REL1_DIR/MANIFEST"

REL2_DIR="$RELEASES/spira-${TS2}"
mkdir -p "$REL2_DIR/spira"
printf 'commit %s\ntimestamp %s\n' "$COMMIT2" "$TS2" > "$REL2_DIR/MANIFEST"

# ---------------------------------------------------------------------------
# run_skew: check in a minimal isolated environment.
# ---------------------------------------------------------------------------
run_skew() {
    local run_dir; run_dir="$(mktemp -d "$TMP/run-XXXXX")"
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$REPO/spira" \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$run_dir" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_RELEASES="$RELEASES" \
        "${@}" \
        bash "$HERE/skew.sh" check 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# ===========================================================================
echo
echo "positive control — activated is NOT latest → NOT-LATEST reported:"
# ===========================================================================
rm -f "$RELEASES/current"
ln -s "spira-${TS1}" "$RELEASES/current"

stale_out="$(run_skew)"; stale_rc=$?
is   "not-latest: exits 1"                    "1"                         "$stale_rc"
want "not-latest: NOT-LATEST finding present" "NOT-LATEST"                "$stale_out"
want "not-latest: names activated release"    "spira-${TS1}"              "$stale_out"
want "not-latest: names latest tag"           "spira-release-spira-${TS2}" "$stale_out"

# ===========================================================================
echo
echo "positive control — MANIFEST commit does not match release tag:"
# ===========================================================================
rm -f "$RELEASES/current"
ln -s "spira-${TS2}" "$RELEASES/current"
WRONG_COMMIT="deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
printf 'commit %s\ntimestamp %s\n' "$WRONG_COMMIT" "$TS2" > "$REL2_DIR/MANIFEST"

mismatch_out="$(run_skew)"; mismatch_rc=$?
is   "manifest-mismatch: exits 1"                          "1"                    "$mismatch_rc"
want "manifest-mismatch: MANIFEST-MISMATCH finding present" "MANIFEST-MISMATCH"   "$mismatch_out"
want "manifest-mismatch: names the MANIFEST commit"        "$WRONG_COMMIT"        "$mismatch_out"
want "manifest-mismatch: names the tag commit"             "$COMMIT2"             "$mismatch_out"

# Restore MANIFEST to correct state.
printf 'commit %s\ntimestamp %s\n' "$COMMIT2" "$TS2" > "$REL2_DIR/MANIFEST"

# ===========================================================================
echo
echo "silence when activated is latest and MANIFEST matches:"
# ===========================================================================
clean_out="$(run_skew)"; clean_rc=$?
is     "clean: exits 0"                   "0"                  "$clean_rc"
want   "clean: 'in effect' message"       "in effect"          "$clean_out"
nowant "clean: no NOT-LATEST"             "NOT-LATEST"         "$clean_out"
nowant "clean: no MANIFEST-MISMATCH"      "MANIFEST-MISMATCH"  "$clean_out"

# ===========================================================================
echo
echo "CANNOT-CHECK cases — exit 3, not a silent pass:"
# ===========================================================================
no_releases_out="$(run_skew SPIRA_RELEASES='')"; no_releases_rc=$?
is "no SPIRA_RELEASES: exits 3" "3" "$no_releases_rc"

no_current_dir="$TMP/no-current-releases"
mkdir -p "$no_current_dir"
no_current_out="$(run_skew SPIRA_RELEASES="$no_current_dir")"; no_current_rc=$?
is "no current symlink: exits 3" "3" "$no_current_rc"

no_manifest_dir="$TMP/no-manifest-releases"
mkdir -p "$no_manifest_dir/spira-${TS2}/spira"
ln -s "spira-${TS2}" "$no_manifest_dir/current"
no_manifest_out="$(run_skew SPIRA_RELEASES="$no_manifest_dir")"; no_manifest_rc=$?
is "no MANIFEST: exits 3" "3" "$no_manifest_rc"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
