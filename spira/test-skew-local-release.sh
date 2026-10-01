#!/usr/bin/env bash
# tier: T2
# covers: spira/skew.sh
#
# test-skew-local-release.sh — skew.sh under queue.local (sp-9thdw). Nothing landing
# through this box's own home repo ever gets a release tag: `queue land-local` builds,
# verifies and activates a commit straight off local/main, with no push to a forge. Before
# this bead, `check()`'s tag machinery answered the only question it knew how to ask — "does
# some tag's commit match?" — and, on a box whose GH_INTAKE_REPO fallback still resolved to
# a real (but unrelated) public repository, that produced CANNOT-VERIFY forever, never a
# clean pass, for a release that was in fact exactly right (sp-9thdw's own repro:
# "CANNOT-VERIFY no release tag found for 03fe8a8ae…").
#
# THE POSITIVE CONTROLS ARE FIRST (law-absence-needs-a-positive-control):
#   1. An activated release BEHIND local/main's tip → LOCAL-BEHIND, exit 1.
#   2. A tampered release (fails `release verify`) → LOCAL-TAMPERED, exit 1, even when the
#      commit matches the tip.
#   3. An unexplained divergence (neither the tip nor a recorded hotfix) → CANNOT-VERIFY,
#      exit 3 — fail closed, never silently clean.
# Then the two clean paths: an exact MATCH, and a recorded standing HOTFIX.
#
# Also covers the two refresh() fixes this bead makes:
#   - refresh with no repo argument (the periodic timer's own call) must resolve the home
#     repo's checkout through the repo map, not through $SPIRA_REPO — which, once a release
#     is activated, names the release directory itself (never a git checkout) rather than
#     the checkout that landed it.
#   - a queue.local refresh must not alarm LOCAL-SKEW when the running commit is a recorded
#     standing hotfix; that is the mechanism's own point.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-skew-local-release.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home"

# ---------------------------------------------------------------------------
# REPO: a queue.local fixture. trunk is the working branch; local/main is the landed ref
# (the repo map's declared base), matching the shape test-skew-refresh.sh's container tier uses.
# C1 -> C2 on local/main (C2 is the tip). C3 branches off C1 on a side branch and is never
# merged, so it is a genuine divergence: neither C2's ancestor nor its descendant.
# ---------------------------------------------------------------------------
REPO="$TMP/repo"
git init -q -b trunk "$REPO"
git -C "$REPO" config user.email "test@test"
git -C "$REPO" config user.name "test"
mkdir -p "$REPO/spira"
printf '#!/usr/bin/env bash\n' > "$REPO/spira/lib.sh"
git -C "$REPO" add spira/
git -C "$REPO" commit -q -m "base"
C1="$(git -C "$REPO" rev-parse HEAD)"

printf '# v2\n' >> "$REPO/spira/lib.sh"
git -C "$REPO" add spira/lib.sh
git -C "$REPO" commit -q -m "advance"
C2="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" branch local/main trunk

git -C "$REPO" checkout -qb side "$C1"
printf '# side\n' > "$REPO/spira/side.sh"
git -C "$REPO" add spira/side.sh
git -C "$REPO" commit -q -m "diverge"
C3="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" checkout -q trunk
git -C "$REPO" branch -D side >/dev/null 2>&1

RMAP="$TMP/fixture-repo-list"
printf 'lfixq | %s | queue.local | local/main | | |\n' "$REPO" > "$RMAP"

# ---------------------------------------------------------------------------
# RELEASES: one release directory per commit, named by its sha (queue.local's own naming —
# release build's directories are always spira-releases/<sha>, never a tagged/timestamped
# name). `current` is repointed per scenario.
# ---------------------------------------------------------------------------
RELEASES="$TMP/releases"
mk_release() {
    local sha="$1"
    mkdir -p "$RELEASES/$sha"
    printf 'commit %s\nbuilt 2026-09-30T00:00:00Z\nrepo lfixq\n' "$sha" > "$RELEASES/$sha/MANIFEST"
}
reset_releases() {
    rm -rf "$RELEASES"; mkdir -p "$RELEASES"
    mk_release "$C1"; mk_release "$C2"; mk_release "$C3"
}
activate() { rm -f "$RELEASES/current"; ln -s "$1" "$RELEASES/current"; }

# ---------------------------------------------------------------------------
# STUB release: verify and status, controlled by two files in STUBCTL. Neither `cargo` nor
# a real store is needed — this suite is about skew's own glue, not the release crate
# (which has its own unit tests).
# ---------------------------------------------------------------------------
STUBBIN="$TMP/stubbin"; mkdir -p "$STUBBIN"
STUBCTL="$TMP/stubctl"; mkdir -p "$STUBCTL"
cat > "$STUBBIN/release" <<STUBREL
#!/usr/bin/env bash
case "\${1:-}" in
    verify)
        if [ -f "$STUBCTL/verify_fail" ]; then
            echo "release \$2 FAILS verify:"
            echo "  \$(cat "$STUBCTL/verify_fail")"
            exit 1
        fi
        echo "release \$2 verifies"
        exit 0
        ;;
    status)
        if [ -f "$STUBCTL/status_output" ]; then
            cat "$STUBCTL/status_output"
        else
            printf 'current stub\n'
        fi
        exit 0
        ;;
    *) exit 1 ;;
esac
STUBREL
chmod +x "$STUBBIN/release"
reset_stub() { rm -f "$STUBCTL/verify_fail" "$STUBCTL/status_output"; }

# A quiet mail: the LOCAL-BEHIND refresh scenario below escalates unconditionally
# (queue.local's own _refresh_check_only, unchanged by this bead), and this suite is not
# about the mail path — that is test-skew-escalate.sh's job.
printf '#!/usr/bin/env bash\nexit 0\n' > "$STUBBIN/mail"
chmod +x "$STUBBIN/mail"

run_check() {
    local run_dir; run_dir="$(mktemp -d "$TMP/run-XXXXX")"
    env -i PATH="$STUBBIN:$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_REPO="$REPO" \
        SPIRA_HOME_REPO="lfixq" \
        SPIRA_REPO_MAP="$RMAP" \
        SPIRA_RUN="$run_dir" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_RELEASES="$RELEASES" \
        "${@}" \
        skew.sh check 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# ===========================================================================
echo
echo "positive control — activated release BEHIND local/main's tip -> LOCAL-BEHIND, exits 1:"
# ===========================================================================
reset_releases; reset_stub
activate "$C1"

behind_out="$(run_check)"; behind_rc=$?
is   "behind: exits 1"                      "1"            "$behind_rc"
want "behind: LOCAL-BEHIND finding present" "LOCAL-BEHIND" "$behind_out"
want "behind: names the activated sha"      "$C1"          "$behind_out"
want "behind: names local/main's tip"       "$C2"          "$behind_out"
want "behind: counts 1 commit"              "1 commit"     "$behind_out"
nowant "behind: no CANNOT-VERIFY"           "CANNOT-VERIFY" "$behind_out"

# ===========================================================================
echo
echo "positive control — release verify fails (tampered) -> LOCAL-TAMPERED, exits 1 even when the commit matches the tip:"
# ===========================================================================
reset_releases; reset_stub
activate "$C2"
printf 'bin/spira-config sha256 mismatch' > "$STUBCTL/verify_fail"

tampered_out="$(run_check)"; tampered_rc=$?
is   "tampered: exits 1"                        "1"              "$tampered_rc"
want "tampered: LOCAL-TAMPERED finding present" "LOCAL-TAMPERED" "$tampered_out"
want "tampered: carries the verify failure"     "sha256 mismatch" "$tampered_out"
reset_stub

# ===========================================================================
echo
echo "positive control — unexplained divergence (no hotfix recorded) -> CANNOT-VERIFY, exits 3, never a clean pass:"
# ===========================================================================
reset_releases; reset_stub
activate "$C3"

cannot_out="$(run_check)"; cannot_rc=$?
is   "cannot-verify: exits 3"                    "3"               "$cannot_rc"
want "cannot-verify: CANNOT-VERIFY finding present" "CANNOT-VERIFY" "$cannot_out"
nowant "cannot-verify: not reported as in effect" "in effect"      "$cannot_out"

# ===========================================================================
echo
echo "clean — MATCH: activated release is local/main's tip, verify passes -> exits 0:"
# ===========================================================================
reset_releases; reset_stub
activate "$C2"

match_out="$(run_check)"; match_rc=$?
is     "match: exits 0"              "0"           "$match_rc"
want   "match: 'in effect' message"  "in effect"   "$match_out"
want   "match: names local/main"     "local/main"  "$match_out"
nowant "match: no LOCAL-BEHIND"      "LOCAL-BEHIND" "$match_out"
nowant "match: no CANNOT-VERIFY"     "CANNOT-VERIFY" "$match_out"

# ===========================================================================
echo
echo "clean — HOTFIX: activated release diverges from the tip, but release status records it as the standing hotfix -> exits 0, not a finding:"
# ===========================================================================
reset_releases; reset_stub
activate "$C3"
printf 'RUNNING UNLANDED %s: stop the world fix (since 2026-09-30T00:00:00Z)\n' "$C3" > "$STUBCTL/status_output"

hotfix_out="$(run_check)"; hotfix_rc=$?
is     "hotfix: exits 0"                 "0"             "$hotfix_rc"
want   "hotfix: 'in effect' message"     "in effect"     "$hotfix_out"
want   "hotfix: names the standing hotfix" "standing hotfix" "$hotfix_out"
nowant "hotfix: no LOCAL-BEHIND"         "LOCAL-BEHIND"  "$hotfix_out"
nowant "hotfix: no CANNOT-VERIFY"        "CANNOT-VERIFY" "$hotfix_out"
reset_stub

# ===========================================================================
echo
echo "a hotfix recorded for a DIFFERENT sha does not excuse this one -> still CANNOT-VERIFY:"
# ===========================================================================
reset_releases; reset_stub
activate "$C3"
printf 'RUNNING UNLANDED %s: an unrelated hotfix (since 2026-09-30T00:00:00Z)\n' "$C1" > "$STUBCTL/status_output"

wronghotfix_out="$(run_check)"; wronghotfix_rc=$?
is   "wrong-hotfix: exits 3"                 "3"             "$wronghotfix_rc"
want "wrong-hotfix: CANNOT-VERIFY finding"   "CANNOT-VERIFY" "$wronghotfix_out"
reset_stub

# ===========================================================================
echo
echo "refresh — no repo argument resolves the home repo's checkout via the repo map, not \$SPIRA_REPO (sp-9thdw's production repro: \$SPIRA_REPO names the release directory, which has no .git):"
# ===========================================================================
# A stand-in "release directory": a plain directory with no .git, exactly what conf.sh
# derives SPIRA_REPO to once a release is activated (SPIRA_HOME/.. under spira-releases/).
FAKE_RELEASE_DIR="$TMP/fake-release-dir"; mkdir -p "$FAKE_RELEASE_DIR/spira"

reset_releases; reset_stub
activate "$C2"   # running matches the tip — refresh should report "nothing to deploy"

refresh_run="$(mktemp -d "$TMP/run-XXXXX")"
# SPIRA_HOME, NOT SPIRA_REPO: conf.sh derives SPIRA_REPO from SPIRA_HOME's own parent when
# unset, and only THAT natural derivation makes SPIRA_REPO equal SPIRA_REPO_DERIVED, which
# is what makes repo_root fall through to the repo map. Setting SPIRA_REPO directly here
# would instead trip repo_root's "somebody set this on purpose" override and hand back
# $FAKE_RELEASE_DIR verbatim — exactly the ambiguity the real unit does NOT create, since it
# sets only SPIRA_RELEASE and PATH.
refresh_out="$(env -i PATH="$STUBBIN:$PATH" \
    HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$FAKE_RELEASE_DIR/spira" \
    SPIRA_HOME_REPO="lfixq" \
    SPIRA_REPO_MAP="$RMAP" \
    SPIRA_RUN="$refresh_run" \
    SPIRA_DOLT_DATA="" \
    SPIRA_TESTDB_DATA="" \
    SPIRA_RELEASES="$RELEASES" \
    skew.sh refresh 2>&1)"; refresh_rc=$?
is     "refresh no-arg: exits 0"                 "0"                  "$refresh_rc"
want   "refresh no-arg: reached queue.local check-only" "nothing to deploy" "$refresh_out"
nowant "refresh no-arg: not a git checkout error"       "is not a git checkout" "$refresh_out"

# Same fixture, but the running release is now behind — must still route through
# _refresh_check_only (report LOCAL-SKEW), never fall through to the checkout-git-fetch
# release-mode branch (which would fail on $FAKE_RELEASE_DIR having no .git).
activate "$C1"
refresh_run2="$(mktemp -d "$TMP/run-XXXXX")"
refresh_out2="$(env -i PATH="$STUBBIN:$PATH" \
    HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$FAKE_RELEASE_DIR/spira" \
    SPIRA_HOME_REPO="lfixq" \
    SPIRA_REPO_MAP="$RMAP" \
    SPIRA_RUN="$refresh_run2" \
    SPIRA_DOLT_DATA="" \
    SPIRA_TESTDB_DATA="" \
    SPIRA_RELEASES="$RELEASES" \
    skew.sh refresh 2>&1)"; refresh_rc2=$?
is     "refresh no-arg, behind: exits 1"           "1"          "$refresh_rc2"
want   "refresh no-arg, behind: LOCAL-SKEW"        "LOCAL-SKEW" "$refresh_out2"
nowant "refresh no-arg, behind: not a git checkout error" "is not a git checkout" "$refresh_out2"

# ===========================================================================
echo
echo "refresh — a running commit that is a recorded standing hotfix is not LOCAL-SKEW:"
# ===========================================================================
reset_releases
activate "$C3"
printf 'RUNNING UNLANDED %s: stop the world fix (since 2026-09-30T00:00:00Z)\n' "$C3" > "$STUBCTL/status_output"

refresh_run3="$(mktemp -d "$TMP/run-XXXXX")"
refresh_out3="$(env -i PATH="$STUBBIN:$PATH" \
    HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="lfixq" \
    SPIRA_REPO_MAP="$RMAP" \
    SPIRA_RUN="$refresh_run3" \
    SPIRA_DOLT_DATA="" \
    SPIRA_TESTDB_DATA="" \
    SPIRA_RELEASES="$RELEASES" \
    skew.sh refresh "$REPO" 2>&1)"; refresh_rc3=$?
is     "refresh hotfix: exits 0"              "0"                 "$refresh_rc3"
want   "refresh hotfix: names the hotfix"     "standing hotfix"   "$refresh_out3"
nowant "refresh hotfix: no LOCAL-SKEW"        "LOCAL-SKEW"        "$refresh_out3"
reset_stub

echo
tl_summary
