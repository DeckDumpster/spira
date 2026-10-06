#!/usr/bin/env bash
#
# test-conf.sh — the production path comes from configuration, never from a literal.
#
# WHAT THIS SUITE IS FOR
# ----------------------
# Every path this harness touches must come from conf.sh, not from a literal baked into
# a script. A literal is how five programs come to disagree, and the half nobody notices
# is wrong is the one that simply runs against the wrong checkout. This applies above
# all to SPIRA_PROD: the path systemd will execute is not a build-time constant.
#
# THE POSITIVE CONTROL IS FIRST. Before asserting that SPIRA_PROD can be overridden,
# prove that conf.sh sets it at all: a conf.sh that defines nothing still passes every
# "can override" assertion. Source conf.sh under a controlled environment and confirm the
# key appears in the exported set.
#
# WHAT IS VERIFIED
# 1. SPIRA_PROD has a non-empty default when no env or config file sets it.
# 2. An environment-set SPIRA_PROD wins over the file-derived default.
# 3. A config-file-set SPIRA_PROD wins over the derived default, but env still wins.
# 4. The default for SPIRA_PROD derives from other configuration keys (SPIRA_WORKSPACES,
#    SPIRA_HOME_REPO) — it is not a literal — verified by asserting that changing
#    SPIRA_WORKSPACES changes the default.
# 5. SPIRA_INSTANCE defaults to 'prod' when unset.
# 6. SPIRA_DB and SPIRA_RUN are instance-qualified: prod gets the unqualified path
#    (backwards-compatible), a named instance gets a distinct sidecar path.
#
# defect: sp-gsmx.2, sp-0v26
# tier: T1
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
isne()   { [ "$2" != "$3" ] && ok "$1" || bad "$1" "wanted NOT [$2] got [$3]"; }

echo "test-conf.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# A minimal harness tree so conf.sh resolves sensibly.
HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"
ln -s "$HERE/conf.sh" "$HARNESS/spira/conf.sh"
# SPIRA_HOME IS THE HOME NOW (locate_home no longer searches): every binary reads
# <home>/conf.d directly, so this fixture needs one too (sfail round 3, pattern 1).
ln -s "$HERE/conf.d" "$HARNESS/spira/conf.d"
printf '# empty\n' > "$HARNESS/spira/repo-map.example"
printf '# empty\n' > "$HARNESS/spira/watchers"

# THE ONE SOURCE OF CONFIG applies to conf.sh too: every registered key below is now read
# only from SPIRA_TOML, never the environment, and there is no legacy spira.conf tier left
# to auto-convert (spira-config::locate drops it entirely — "no legacy spira.conf, no
# shipped example"). SPIRA_WATCHERS is registered and fixed for the whole suite, so it is
# declared once into the suite's own override file (the second SPIRA_TOML layer testlib.sh
# already set up).
tl_config SPIRA_WATCHERS="$HARNESS/spira/watchers"

# Load conf.sh in a subprocess and print the value of the requested key. "$@" simulates an
# override: a registered SPIRA_* name is written into a fresh, call-scoped toml layer (the
# same KEY -> spira.key convention tl_config uses) so conf.sh sees it through SPIRA_TOML;
# a non-registered identity var (SPIRA_REPO) or a plain env var (XDG_DATA_HOME) still rides
# the env -i prefix, since neither is a registered config key.
conf_val() {
    local key="$1"; shift
    local override="$TMP/conf_val-override.toml"
    printf '[spira]\n' > "$override"
    local -a env_extra=()
    local kv k v d
    for kv in "$@"; do
        k="${kv%%=*}"; v="${kv#*=}"
        case "$k" in
            SPIRA_REPO|XDG_DATA_HOME)
                env_extra+=("$kv") ;;
            *)
                d="spira.$(printf '%s' "${k#SPIRA_}" | tr '[:upper:]' '[:lower:]')"
                spira-config set "$d" "$v" "$override" >/dev/null ;;
        esac
    done
    env -i SPIRA_TOML="$SPIRA_TOML:$override" "${env_extra[@]}" PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent SPIRA_HOME="$HARNESS/spira" \
        bash -c ". '$HARNESS/spira/conf.sh'; printf '%s' \"\${${key}:-}\"" 2>/dev/null
    rm -f "$override"
}

# ==========================================================================
echo
echo "positive control — SPIRA_PROD is set by conf.sh:"
# ==========================================================================
prod_default="$(conf_val SPIRA_PROD)"
isne "SPIRA_PROD default is non-empty" "" "$prod_default"

# ==========================================================================
echo
echo "env wins — SPIRA_PROD set in environment overrides the default:"
# ==========================================================================
custom_prod="$TMP/my-own-prod/spira"
got="$(conf_val SPIRA_PROD SPIRA_PROD="$custom_prod")"
is "env-set SPIRA_PROD wins" "$custom_prod" "$got"

# ==========================================================================
echo
echo "default is derived — changing SPIRA_WORKSPACES changes the default:"
# ==========================================================================
ws_a="$TMP/workspace-a"
ws_b="$TMP/workspace-b"
prod_a="$(conf_val SPIRA_PROD SPIRA_WORKSPACES="$ws_a")"
prod_b="$(conf_val SPIRA_PROD SPIRA_WORKSPACES="$ws_b")"
isne "default changes with SPIRA_WORKSPACES" "$prod_a" "$prod_b"
want "default includes SPIRA_WORKSPACES path" "$ws_a" "$prod_a"
want "default includes SPIRA_WORKSPACES path" "$ws_b" "$prod_b"

# DELETED: "config file — SPIRA_PROD from spira.conf wins over derived default" and "env
# still overrides the config file". Both tested the legacy spira.conf tier, which
# spira-config::locate no longer searches at all under the one-source-of-config law ("no
# legacy spira.conf, no shipped example" — SPIRA_TOML is the only file a caller sees); a
# SPIRA_CONF path pointing at a spira.conf-format file is no longer converted into a config
# layer by anything, so there is no behaviour left here to assert.

# ==========================================================================
echo
echo "SPIRA_INSTANCE — defaults to 'prod' when unset, qualifies SPIRA_DB and SPIRA_RUN:"
# ==========================================================================
# Positive control: conf.sh must set SPIRA_INSTANCE at all.
inst_default="$(conf_val SPIRA_INSTANCE)"
is "SPIRA_INSTANCE default is 'prod'" "prod" "$inst_default"

# An explicit instance name wins.
inst_got="$(conf_val SPIRA_INSTANCE SPIRA_INSTANCE=test)"
is "env-set SPIRA_INSTANCE wins" "test" "$inst_got"

# For prod (unset), SPIRA_DB must contain 'spira' WITHOUT a dash-qualified suffix,
# so that every existing box keeps the path it already has.
db_prod="$(conf_val SPIRA_DB)"
nowant "prod SPIRA_DB has no instance suffix" "-prod" "$db_prod"
want   "prod SPIRA_DB contains 'spira'"       "spira" "$db_prod"

# For a named non-prod instance, SPIRA_DB must be distinct from the prod path.
db_test="$(conf_val SPIRA_DB SPIRA_INSTANCE=test)"
isne "test SPIRA_DB differs from prod SPIRA_DB" "$db_prod" "$db_test"
want "test SPIRA_DB is instance-qualified"       "spira-test" "$db_test"

# For prod (unset), SPIRA_RUN must not carry an instance suffix.
run_prod="$(conf_val SPIRA_RUN)"
nowant "prod SPIRA_RUN has no instance suffix" "-prod" "$run_prod"

# For a named non-prod instance, SPIRA_RUN must be distinct.
run_test="$(conf_val SPIRA_RUN SPIRA_INSTANCE=test)"
isne "test SPIRA_RUN differs from prod SPIRA_RUN" "$run_prod" "$run_test"
want "test SPIRA_RUN is instance-qualified"        "spira-test" "$run_test"

# SPIRA_INSTANCE is exported so child processes see it without re-sourcing conf.sh.
exported="$(env -i SPIRA_TOML="$SPIRA_TOML" PATH="$PATH" HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent SPIRA_HOME="$HARNESS/spira" \
    bash -c ". '$HARNESS/spira/conf.sh'; env | grep '^SPIRA_INSTANCE='" 2>/dev/null)"
want "SPIRA_INSTANCE is exported" "SPIRA_INSTANCE=" "$exported"

# ==========================================================================
echo
echo "SPIRA_RUN never defaults inside SPIRA_REPO, writable or not (sp-9hwim, design"
echo "runtime-is-a-release #5 — nothing reads or writes the checkout at runtime):"
# ==========================================================================
# A writable SPIRA_REPO must NOT produce a path inside it. This used to fall back to
# "$SPIRA_REPO/.runtime/spira"; that branch is gone, so a writable checkout gets the same
# XDG default an unwritable one always got. The checkout has no working tree at all once
# the running system cuts over to spira-releases/<sha>, so a default that could still land
# there would resolve to a path that does not exist.
WRITABLE_REPO="$TMP/writable-repo"
mkdir -p "$WRITABLE_REPO"
run_writable="$(conf_val SPIRA_RUN SPIRA_REPO="$WRITABLE_REPO" XDG_DATA_HOME="$TMP/xdg-data")"
nowant "writable SPIRA_REPO: SPIRA_RUN is NOT inside the repo" "$WRITABLE_REPO" "$run_writable"
want   "writable SPIRA_REPO: SPIRA_RUN still uses XDG fallback" "$TMP/xdg-data" "$run_writable"

# An unwritable SPIRA_REPO gets the identical XDG default — same code path, no branch.
READONLY_REPO="$TMP/readonly-repo"
mkdir -p "$READONLY_REPO"
chmod a-w "$READONLY_REPO"
run_readonly="$(conf_val SPIRA_RUN SPIRA_REPO="$READONLY_REPO" XDG_DATA_HOME="$TMP/xdg-data")"
chmod u+w "$READONLY_REPO"
nowant "read-only SPIRA_REPO: SPIRA_RUN not inside repo" "$READONLY_REPO" "$run_readonly"
want   "read-only SPIRA_REPO: SPIRA_RUN uses XDG fallback" "$TMP/xdg-data" "$run_readonly"
is     "writable and read-only SPIRA_REPO produce the identical SPIRA_RUN default" \
       "$run_readonly" "$run_writable"

# ==========================================================================
echo
echo "root workspace — SPIRA_REPO at filesystem root produces no double slashes:"
# ==========================================================================
# When the repo is bind-mounted at /workspace, dirname gives "/" and a naive
# "$SPIRA_WORKSPACES/foo" yields "//foo". Verify both derivation sites clean.
# Positive control: with a normal path, the derivation must produce something.
testdb_normal="$(conf_val SPIRA_TESTDB_DATA SPIRA_WORKSPACES="$TMP/workspaces")"
want "SPIRA_TESTDB_DATA is set with normal SPIRA_WORKSPACES" "$TMP/workspaces" "$testdb_normal"

testdb_root="$(conf_val SPIRA_TESTDB_DATA SPIRA_REPO=/workspace)"
nowant "SPIRA_TESTDB_DATA has no double slash when SPIRA_REPO=/workspace" "//" "$testdb_root"
want   "SPIRA_TESTDB_DATA starts with /beads-test when SPIRA_REPO=/workspace" "/beads-test" "$testdb_root"

prod_root="$(conf_val SPIRA_PROD SPIRA_REPO=/workspace)"
nowant "SPIRA_PROD has no double slash when SPIRA_REPO=/workspace" "//" "$prod_root"
want   "SPIRA_PROD is non-empty when SPIRA_REPO=/workspace" "/" "$prod_root"

# ==========================================================================
echo
echo "artifact deployment — SPIRA_WORKSPACES is grandparent of SPIRA_REPO, not parent:"
# ==========================================================================
# In artifact mode SPIRA_REPO is a release dir inside spira-releases; the correct
# workspaces directory is two levels up, not one. Without the fix, SPIRA_WORKSPACES
# would be the releases directory, causing SPIRA_RELEASES to double.
# Positive control: with old (one-level) derivation, SPIRA_WORKSPACES would include
# "spira-releases" in the path; the fix must not.
ART_RELEASES="$TMP/art-releases"
ART_RELEASE="$ART_RELEASES/spira-20260912T120000Z"
mkdir -p "$ART_RELEASE"
ws_art="$(conf_val SPIRA_WORKSPACES SPIRA_REPO="$ART_RELEASE")"
is    "artifact: SPIRA_WORKSPACES is workspaces grandparent" "$TMP" "$ws_art"
nowant "artifact: SPIRA_WORKSPACES excludes releases subdir"  "art-releases" "$ws_art"

# ==========================================================================
echo
echo "SPIRA_HOME_REPO from worktree — resolves to main repo name, not worktree name:"
# ==========================================================================
# Positive control first: with a plain checkout the name is the repo's basename.
# The regression: from a worktree whose directory name differs from the main repo,
# the old code (basename "$SPIRA_REPO") returned the worktree directory name instead.
WTEST_MAIN="$TMP/wtest-main-repo"
git init -q "$WTEST_MAIN"
git -C "$WTEST_MAIN" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
WTEST_WT="$TMP/wtest-worktree-xyzzy"   # name that must NOT appear as SPIRA_HOME_REPO
git -C "$WTEST_MAIN" worktree add -q "$WTEST_WT" -b wt-branch
mkdir -p "$WTEST_WT/spira"
ln -sf "$HERE/conf.sh" "$WTEST_WT/spira/conf.sh"
ln -sf "$HERE/conf.d" "$WTEST_WT/spira/conf.d"
wt_got="$(env -i SPIRA_TOML="$SPIRA_TOML" PATH="$PATH" HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent SPIRA_HOME="$WTEST_WT/spira" \
    bash -c ". '$WTEST_WT/spira/conf.sh'; printf '%s' \"\${SPIRA_HOME_REPO:-}\"" 2>/dev/null)"
is    "worktree: SPIRA_HOME_REPO equals main repo name" "wtest-main-repo" "$wt_got"
isne  "worktree: SPIRA_HOME_REPO is not the worktree dir name" "wtest-worktree-xyzzy" "$wt_got"

# ==========================================================================
echo
echo "installed release — SPIRA_HOME_REPO comes from MANIFEST's stamp, not the release dir name:"
# ==========================================================================
# An installed release is an unpacked tarball named spira-<timestamp>, which changes on
# every upgrade (law-scope-is-a-runtime-key). basename(SPIRA_REPO) must never be used
# here — that was sp-j4vi0: every upgrade made every existing bead unclaimable.
RELEASE_DIR="$TMP/spira-20990101T000000Z"
mkdir -p "$RELEASE_DIR"

# T1: with the stamp, SPIRA_HOME_REPO resolves to the stamped identity.
printf 'commit 0000000000000000000000000000000000000000\ntimestamp 20990101T000000Z\nrepo spira\n' \
    > "$RELEASE_DIR/MANIFEST"
release_got="$(conf_val SPIRA_HOME_REPO SPIRA_REPO="$RELEASE_DIR")"
is   "stamped release: SPIRA_HOME_REPO resolves to the stamp (spira)" "spira" "$release_got"
isne "stamped release: SPIRA_HOME_REPO is not the release dir name" "spira-20990101T000000Z" "$release_got"

# Without the stamp, conf.sh refuses to fall back to the directory name.
rm -f "$RELEASE_DIR/MANIFEST"
unstamped_got="$(conf_val SPIRA_HOME_REPO SPIRA_REPO="$RELEASE_DIR")"
isne "unstamped release: SPIRA_HOME_REPO is not the release dir name" "spira-20990101T000000Z" "$unstamped_got"
is   "unstamped release: SPIRA_HOME_REPO is refused (empty), not guessed" "" "$unstamped_got"

# A MANIFEST present but without a `repo` line is the same as no stamp at all.
printf 'commit 0000000000000000000000000000000000000000\ntimestamp 20990101T000000Z\n' \
    > "$RELEASE_DIR/MANIFEST"
norepo_got="$(conf_val SPIRA_HOME_REPO SPIRA_REPO="$RELEASE_DIR")"
isne "MANIFEST without repo line: SPIRA_HOME_REPO is not the release dir name" "spira-20990101T000000Z" "$norepo_got"
is   "MANIFEST without repo line: SPIRA_HOME_REPO is refused (empty)" "" "$norepo_got"

# ==========================================================================
echo
echo "installed release — SPIRA_RELEASE_REPO comes from MANIFEST's release-repo when unset:"
# ==========================================================================
# A release install is never sourceless: the tarball records the forge it was published from,
# and skew.sh's release-currency check reads SPIRA_RELEASE_REPO (exit 3 without one).
printf 'commit 0000000000000000000000000000000000000000\ntimestamp 20990101T000000Z\nrepo spira\nrelease-repo owner/publisher\n' \
    > "$RELEASE_DIR/MANIFEST"
is "stamped release: SPIRA_RELEASE_REPO is the MANIFEST's release-repo" "owner/publisher" \
   "$(conf_val SPIRA_RELEASE_REPO SPIRA_REPO="$RELEASE_DIR")"
is "an explicit SPIRA_RELEASE_REPO still wins" "/srv/releases" \
   "$(conf_val SPIRA_RELEASE_REPO SPIRA_REPO="$RELEASE_DIR" SPIRA_RELEASE_REPO=/srv/releases)"
is "and so does SPIRA_GH_INTAKE_REPO, as before" "owner/intake" \
   "$(conf_val SPIRA_RELEASE_REPO SPIRA_REPO="$RELEASE_DIR" SPIRA_GH_INTAKE_REPO=owner/intake)"
rm -f "$RELEASE_DIR/MANIFEST"

# Only a release-named directory is refused. A non-git tree with any other name (a test
# fixture, a scratch copy) keeps its directory name, as before sp-j4vi0; refusing there
# emptied the scope label for every suite that builds one (Concierge round 24).
SCRATCH_DIR="$TMP/scratch-fixture-repo"; mkdir -p "$SCRATCH_DIR"
scratch_got="$(conf_val SPIRA_HOME_REPO SPIRA_REPO="$SCRATCH_DIR")"
is   "non-release non-git dir: SPIRA_HOME_REPO keeps its directory name" "scratch-fixture-repo" "$scratch_got"

# ==========================================================================
echo
# HOOK CONTEXT — git's GIT_DIR in the environment does not move SPIRA_REPO. Every git hook
# runs with GIT_DIR exported, and with it set `git -C <dir> rev-parse --show-toplevel`
# answers <dir> itself, not the checkout's top: conf.sh sourced from a hook (pre-commit ->
# branch-guard.sh -> lib.sh) resolved SPIRA_REPO to <repo>/spira. Seen 2026-09-26 in a
# merge's hook output looking for spira-config one directory too deep.
GH="$TMP/git-harness"
mkdir -p "$GH/spira"
ln -s "$HERE/conf.sh" "$GH/spira/conf.sh"
git -C "$GH" init -q
hook_repo="$(env -i SPIRA_TOML="$SPIRA_TOML" PATH="$PATH" HOME="$TMP/hook-home" SPIRA_CONF=/nonexistent GIT_DIR="$GH/.git" \
    bash -c ". '$GH/spira/conf.sh' >/dev/null 2>&1; printf '%s' \"\$SPIRA_REPO\"" 2>/dev/null)"
is "SPIRA_REPO is the checkout's top even with GIT_DIR exported" "$(cd "$GH" && pwd -P)" "$hook_repo"

tl_summary
