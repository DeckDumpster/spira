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
# Create HOME directory to prevent git write failures in batch isolation
mkdir -p "$TMP/home"

# Create HOME directory that will be used by env -i in run_skew
mkdir -p "$TMP/home"

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
# Setup release template: two unpacked release directories.
# Each test case will get an isolated copy to avoid concurrent mutations.
# ---------------------------------------------------------------------------
RELEASES_TEMPLATE="$TMP/releases-template"
mkdir -p "$RELEASES_TEMPLATE"

REL1_TEMPLATE="$RELEASES_TEMPLATE/spira-${TS1}"
mkdir -p "$REL1_TEMPLATE/spira"
printf 'commit %s\ntimestamp %s\n' "$COMMIT1" "$TS1" > "$REL1_TEMPLATE/MANIFEST"

REL2_TEMPLATE="$RELEASES_TEMPLATE/spira-${TS2}"
mkdir -p "$REL2_TEMPLATE/spira"
printf 'commit %s\ntimestamp %s\n' "$COMMIT2" "$TS2" > "$REL2_TEMPLATE/MANIFEST"

# Create isolated copy for test cases and refresh directory pointers
copy_releases() {
    local dest="$1"
    rm -rf "$dest"
    cp -r "$RELEASES_TEMPLATE" "$dest"
}

reset_releases() {
    copy_releases "$RELEASES"
    REL1_DIR="$RELEASES/spira-${TS1}"
    REL2_DIR="$RELEASES/spira-${TS2}"
}

# Initialize first copy
RELEASES="$TMP/releases"
reset_releases

# ---------------------------------------------------------------------------
# run_skew: check in a minimal isolated environment.
# Each invocation gets its own copy of the releases directory to prevent
# interference between concurrent batch executions.
# ---------------------------------------------------------------------------
run_skew() {
    local run_dir
    run_dir="$(mktemp -d "$TMP/run-XXXXX")"

    # Use the test-specific RELEASES set up by reset_releases, not a new isolated copy.
    # This allows test blocks to control what scenario skew.sh sees.
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
# (no sidecar — commit-based fallback must find the tag and report NOT-LATEST)
# ===========================================================================
reset_releases
rm -f "$RELEASES/current"
ln -s "spira-${TS1}" "$RELEASES/current"
rm -rf "$RELEASES/.tags"

stale_out="$(run_skew)"; stale_rc=$?
is   "not-latest: exits 1"                    "1"                         "$stale_rc"
want "not-latest: NOT-LATEST finding present" "NOT-LATEST"                "$stale_out"
want "not-latest: names activated release"    "spira-${TS1}"              "$stale_out"
want "not-latest: names latest tag"           "spira-release-spira-${TS2}" "$stale_out"

# ===========================================================================
echo
echo "positive control — MANIFEST commit does not match release tag (sidecar present):"
# (sidecar identifies the tag; MANIFEST disagrees with what that tag points at)
# ===========================================================================
reset_releases
rm -f "$RELEASES/current"
ln -s "spira-${TS2}" "$RELEASES/current"
mkdir -p "$RELEASES/.tags"
printf 'spira-release-spira-%s\n' "$TS2" > "$RELEASES/.tags/spira-${TS2}"
WRONG_COMMIT="deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
printf 'commit %s\ntimestamp %s\n' "$WRONG_COMMIT" "$TS2" > "$REL2_DIR/MANIFEST"

mismatch_out="$(run_skew)"; mismatch_rc=$?
is   "manifest-mismatch: exits 1"                          "1"                    "$mismatch_rc"
want "manifest-mismatch: MANIFEST-MISMATCH finding present" "MANIFEST-MISMATCH"   "$mismatch_out"
want "manifest-mismatch: names the MANIFEST commit"        "$WRONG_COMMIT"        "$mismatch_out"
want "manifest-mismatch: names the tag commit"             "$COMMIT2"             "$mismatch_out"

# Restore MANIFEST to correct state; remove sidecar.
printf 'commit %s\ntimestamp %s\n' "$COMMIT2" "$TS2" > "$REL2_DIR/MANIFEST"
rm -f "$RELEASES_TEMPLATE/.tags/spira-${TS2}"

# ===========================================================================
echo
echo "silence when activated is latest and MANIFEST matches (via sidecar):"
# ===========================================================================
reset_releases
# Set up state: activate TS2 (latest release) with sidecar
rm -f "$RELEASES/current"
ln -s "spira-${TS2}" "$RELEASES/current"
mkdir -p "$RELEASES/.tags"
printf 'spira-release-spira-%s\n' "$TS2" > "$RELEASES/.tags/spira-${TS2}"

clean_out="$(run_skew)"; clean_rc=$?
is     "clean: exits 0"                   "0"                  "$clean_rc"
want   "clean: 'in effect' message"       "in effect"          "$clean_out"
nowant "clean: no NOT-LATEST"             "NOT-LATEST"         "$clean_out"
nowant "clean: no MANIFEST-MISMATCH"      "MANIFEST-MISMATCH"  "$clean_out"

rm -f "$RELEASES_TEMPLATE/.tags/spira-${TS2}"

# ===========================================================================
echo
echo "silence when activated is latest and MANIFEST matches (commit fallback, no sidecar):"
# ===========================================================================
reset_releases
# Set up state: activate TS2 (latest release) without sidecar
rm -f "$RELEASES/current"
ln -s "spira-${TS2}" "$RELEASES/current"
rm -rf "$RELEASES/.tags"

clean_nosidecar_out="$(run_skew)"; clean_nosidecar_rc=$?
is     "clean-nosidecar: exits 0"              "0"                 "$clean_nosidecar_rc"
want   "clean-nosidecar: 'in effect' message"  "in effect"         "$clean_nosidecar_out"
nowant "clean-nosidecar: no NOT-LATEST"        "NOT-LATEST"        "$clean_nosidecar_out"
nowant "clean-nosidecar: no MANIFEST-MISMATCH" "MANIFEST-MISMATCH" "$clean_nosidecar_out"

# Plain (non-git) SPIRA_REPO for artifact-mode cannot-check cases. After the checkout-mode
# fix, a git SPIRA_REPO with no current symlink delegates to gap rather than exiting 3;
# artifact mode (no .git) still exits 3 — that is the "unknown verdict" path.
NO_GIT_REPO="$TMP/no-git-repo"
mkdir -p "$NO_GIT_REPO"
run_skew_noart() {
    local run_dir releases_dir
    run_dir="$(mktemp -d "$TMP/run-XXXXX")"
    releases_dir="$(mktemp -d "$TMP/releases-XXXXX")"

    # Copy the template to the per-run releases directory (including hidden directories like .tags)
    (cd "$RELEASES_TEMPLATE" && cp -r . "$releases_dir/")

    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$REPO/spira" \
        SPIRA_REPO="$NO_GIT_REPO" \
        SPIRA_RUN="$run_dir" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_RELEASES="$releases_dir" \
        "${@}" \
        bash "$HERE/skew.sh" check 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# ===========================================================================
echo
echo "CANNOT-CHECK cases — exit 3, not a silent pass:"
# ===========================================================================
# conf.sh uses ':=' to fill empty SPIRA_RELEASES with a derived default, so passing ''
# is not the same as "no releases infrastructure". Use an explicit empty directory and
# an artifact-mode (no .git) SPIRA_REPO so the test reaches the cannot-check exit path.

reset_releases
no_current_dir="$TMP/no-current-releases"
mkdir -p "$no_current_dir"
no_releases_out="$(run_skew_noart SPIRA_RELEASES="$no_current_dir")"; no_releases_rc=$?
is "no SPIRA_RELEASES: exits 3" "3" "$no_releases_rc"

# Alias of the above with different framing: artifact mode with no current symlink.
# Both test the same path; kept separate so naming makes the intent clear at a glance.
no_current_out="$(run_skew_noart SPIRA_RELEASES="$no_current_dir")"; no_current_rc=$?
is "no current symlink (artifact mode): exits 3" "3" "$no_current_rc"

no_manifest_dir="$TMP/no-manifest-releases"
mkdir -p "$no_manifest_dir/spira-${TS2}/spira"
ln -s "spira-${TS2}" "$no_manifest_dir/current"
no_manifest_out="$(run_skew SPIRA_RELEASES="$no_manifest_dir")"; no_manifest_rc=$?
is "no MANIFEST: exits 3" "3" "$no_manifest_rc"


# ---------------------------------------------------------------------------
# Artifact mode: SPIRA_REPO has no .git — tags must come from gh release list.
# ---------------------------------------------------------------------------

# Mock gh: outputs GH_RELEASE_LIST when called with "release list"; else fails.
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$MOCK_BIN"
cat > "$MOCK_BIN/gh" <<'GHEOF'
#!/usr/bin/env bash
if [ "${1:-}" = release ] && [ "${2:-}" = list ]; then
    printf '%s\n' "${GH_RELEASE_LIST:-[]}"
    exit 0
fi
exit 1
GHEOF
chmod +x "$MOCK_BIN/gh"

# Non-git directory simulating an unpacked release tarball.
ARTIFACT_REPO="$TMP/artifact-repo"
mkdir -p "$ARTIFACT_REPO"

run_skew_artifact() {
    local run_dir releases_dir
    run_dir="$(mktemp -d "$TMP/run-XXXXX")"
    releases_dir="$(mktemp -d "$TMP/releases-XXXXX")"

    # Copy the template to the per-run releases directory (including hidden directories like .tags)
    (cd "$RELEASES_TEMPLATE" && cp -r . "$releases_dir/")

    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$REPO/spira" \
        SPIRA_REPO="$ARTIFACT_REPO" \
        SPIRA_GH="$MOCK_BIN/gh" \
        SPIRA_RUN="$run_dir" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_RELEASES="$releases_dir" \
        "${@}" \
        bash "$HERE/skew.sh" check 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

GH_LIST='[{"tagName":"spira-release-spira-'"$TS1"'","isDraft":false},{"tagName":"spira-release-spira-'"$TS2"'","isDraft":false}]'

# ===========================================================================
echo
echo "artifact positive control — NOT-LATEST reported via gh release list (SPIRA_GH_INTAKE_REPO):"
# (sidecar present so activated release is identified; gh returns both tags)
# ===========================================================================
reset_releases
rm -f "$RELEASES/current"
ln -s "spira-${TS1}" "$RELEASES/current"
mkdir -p "$RELEASES/.tags"
printf 'spira-release-spira-%s\n' "$TS1" > "$RELEASES/.tags/spira-${TS1}"
printf 'commit %s\ntimestamp %s\n' "$COMMIT1" "$TS1" > "$REL1_DIR/MANIFEST"

art_stale_out="$(run_skew_artifact \
    SPIRA_GH_INTAKE_REPO=test/repo \
    GH_RELEASE_LIST="$GH_LIST")"; art_stale_rc=$?
is   "artifact-not-latest: exits 1"             "1"                          "$art_stale_rc"
want "artifact-not-latest: NOT-LATEST reported" "NOT-LATEST"                 "$art_stale_out"
want "artifact-not-latest: names latest tag"    "spira-release-spira-${TS2}" "$art_stale_out"

# ===========================================================================
echo
echo "artifact mode — SPIRA_RELEASE_REPO set (intake empty) → NOT-LATEST via SPIRA_RELEASE_REPO:"
# Consuming installs set SPIRA_RELEASE_REPO without SPIRA_GH_INTAKE_REPO.
# ===========================================================================
reset_releases
# Set up state: activate TS1 (older release) with sidecar
rm -f "$RELEASES/current"
ln -s "spira-${TS1}" "$RELEASES/current"
mkdir -p "$RELEASES/.tags"
printf 'spira-release-spira-%s\n' "$TS1" > "$RELEASES/.tags/spira-${TS1}"
printf 'commit %s\ntimestamp %s\n' "$COMMIT1" "$TS1" > "$REL1_DIR/MANIFEST"
art_release_repo_out="$(run_skew_artifact \
    SPIRA_RELEASE_REPO=test/repo \
    GH_RELEASE_LIST="$GH_LIST")"; art_release_repo_rc=$?
is   "artifact-release-repo: exits 1"            "1"                          "$art_release_repo_rc"
want "artifact-release-repo: NOT-LATEST reported" "NOT-LATEST"                "$art_release_repo_out"

# ===========================================================================
echo
echo "artifact mode — no SPIRA_GH_INTAKE_REPO and no SPIRA_RELEASE_REPO → exits 3:"
# ===========================================================================
reset_releases
art_no_intake_out="$(run_skew_artifact)"; art_no_intake_rc=$?
is "artifact-no-intake: exits 3" "3" "$art_no_intake_rc"

# ===========================================================================
echo
echo "artifact mode — gh returns no releases → exits 3:"
# ===========================================================================
# GH_RELEASE_LIST defaults to [] in the mock, giving no tags; exit 3 is expected.
reset_releases
art_empty_out="$(run_skew_artifact SPIRA_GH_INTAKE_REPO=test/repo)"; art_empty_rc=$?
is "artifact-gh-empty: exits 3 when gh returns no tags" "3" "$art_empty_rc"

# ===========================================================================
echo
echo "artifact mode — activated is latest → exits 0:"
# ===========================================================================
reset_releases
rm -f "$RELEASES/current"
ln -s "spira-${TS2}" "$RELEASES/current"
mkdir -p "$RELEASES/.tags"
printf 'spira-release-spira-%s\n' "$TS2" > "$RELEASES/.tags/spira-${TS2}"
printf 'commit %s\ntimestamp %s\n' "$COMMIT2" "$TS2" > "$REL2_DIR/MANIFEST"

art_clean_out="$(run_skew_artifact \
    SPIRA_GH_INTAKE_REPO=test/repo \
    GH_RELEASE_LIST="$GH_LIST")"; art_clean_rc=$?
is     "artifact-clean: exits 0"           "0"          "$art_clean_rc"
want   "artifact-clean: in effect message" "in effect"  "$art_clean_out"
nowant "artifact-clean: no NOT-LATEST"     "NOT-LATEST" "$art_clean_out"

# ---------------------------------------------------------------------------
# Checkout mode: no activated release → gap check answers the skew question.
#
# POSITIVE CONTROL (acceptance criterion 3): inject real skew by resetting HEAD behind
# origin/main — check must exit 1 (skew detected), distinct from exit 3 (unknown).
# ---------------------------------------------------------------------------
ORIGIN_CK="$TMP/origin-ck"
CLONE_CK="$TMP/clone-ck"
git init -q "$ORIGIN_CK"
git -C "$ORIGIN_CK" config user.email "test@test"
git -C "$ORIGIN_CK" config user.name "test"
mkdir -p "$ORIGIN_CK/spira"
printf '#!/usr/bin/env bash\n' > "$ORIGIN_CK/spira/lib.sh"
git -C "$ORIGIN_CK" add spira/
git -C "$ORIGIN_CK" commit -q -m "base"
CK_BASE="$(git -C "$ORIGIN_CK" rev-parse HEAD)"
printf '# v2\n' >> "$ORIGIN_CK/spira/lib.sh"
git -C "$ORIGIN_CK" add spira/lib.sh
git -C "$ORIGIN_CK" commit -q -m "advance"
git clone -q "$ORIGIN_CK" "$CLONE_CK"
git -C "$CLONE_CK" config user.email "test@test"
git -C "$CLONE_CK" config user.name "test"
git -C "$CLONE_CK" remote set-head origin --auto >/dev/null 2>&1 || true

RELEASES_CK="$TMP/releases-ck"
mkdir -p "$RELEASES_CK"   # no current symlink — checkout mode

run_skew_checkout() {
    local run_dir; run_dir="$(mktemp -d "$TMP/run-XXXXX")"
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$CLONE_CK" \
        SPIRA_RUN="$run_dir" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_RELEASES="$RELEASES_CK" \
        "${@}" \
        bash "$HERE/skew.sh" check 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# POSITIVE CONTROL: HEAD behind origin/main → skew detected, exits 1.
git -C "$CLONE_CK" reset -q --hard "$CK_BASE"

echo
echo "checkout mode — POSITIVE CONTROL: HEAD behind origin → skew detected (exits 1):"
ck_behind_out="$(run_skew_checkout)"; ck_behind_rc=$?
is   "checkout-behind: exits 1"                  "1"                "$ck_behind_rc"
want "checkout-behind: reports commits behind"   "commit(s) behind" "$ck_behind_out"

# Clean case: HEAD at origin/main → exits 0.
REMOTE_MAIN_CK="$(git -C "$CLONE_CK" symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null)"
git -C "$CLONE_CK" merge --ff-only -q "$REMOTE_MAIN_CK"

echo
echo "checkout mode — clean: HEAD at origin/main → exits 0:"
ck_clean_out="$(run_skew_checkout)"; ck_clean_rc=$?
is   "checkout-clean: exits 0"                    "0"          "$ck_clean_rc"
want "checkout-clean: reports 0 commits behind"   "0 commits"  "$ck_clean_out"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
