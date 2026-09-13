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
# 5. promote.sh contains no hardcoded absolute path for the production checkout.
# 6. SPIRA_INSTANCE defaults to 'prod' when unset.
# 7. SPIRA_DB and SPIRA_RUN are instance-qualified: prod gets the unqualified path
#    (backwards-compatible), a named instance gets a distinct sidecar path.
#
# defect: sp-gsmx.2, sp-0v26
# covers: spira/conf.sh spira/promote.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isne()   { [ "$2" != "$3" ] && ok "$1" || bad "$1" "wanted NOT [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-conf.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# A minimal harness tree so conf.sh resolves sensibly.
HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"
ln -s "$HERE/conf.sh" "$HARNESS/spira/conf.sh"
printf '# empty\n' > "$HARNESS/spira/repo-map.example"
printf '# empty\n' > "$HARNESS/spira/watchers"

# Load conf.sh in a subprocess and print the value of the requested key.
conf_val() {
    local key="$1"; shift
    env -i "$@" PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        bash -c ". '$HARNESS/spira/conf.sh'; printf '%s' \"\${${key}:-}\"" 2>/dev/null
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

# ==========================================================================
echo
echo "config file — SPIRA_PROD from spira.conf wins over derived default:"
# ==========================================================================
CONF_FILE="$TMP/spira.conf"
conf_prod="$TMP/conf-chosen/spira"
printf 'SPIRA_PROD = %s\n' "$conf_prod" > "$CONF_FILE"
got="$(env -i PATH="$PATH" HOME="$TMP/home" SPIRA_CONF="$CONF_FILE" \
    bash -c ". '$HARNESS/spira/conf.sh'; printf '%s' \"\${SPIRA_PROD:-}\"" 2>/dev/null)"
is "config-file SPIRA_PROD wins over derived default" "$conf_prod" "$got"

# env still overrides the config file
override="$TMP/env-overrides/spira"
got="$(env -i PATH="$PATH" HOME="$TMP/home" SPIRA_CONF="$CONF_FILE" SPIRA_PROD="$override" \
    bash -c ". '$HARNESS/spira/conf.sh'; printf '%s' \"\${SPIRA_PROD:-}\"" 2>/dev/null)"
is "env wins over config file" "$override" "$got"

# ==========================================================================
echo
echo "no hardcoded path — promote.sh does not contain a literal SPIRA_PROD value:"
# ==========================================================================
# Inventory.sh already checks for operator-specific path prefixes in tracked files, so
# this is a belt-and-braces check: verify that promote.sh itself has no hardcoded
# production path. The positive control: verify promote.sh IS readable before asserting.
[ -f "$HERE/promote.sh" ] && ok "promote.sh is present" || {
    bad "promote.sh is present" "file not found at $HERE/promote.sh"; }

# grep for any literal path that looks like a fixed production directory —
# something like /path/to/something-prod — which would be an inventory.sh violation anyway,
# but this makes the specific property explicit. We look for a hard string of "-prod/"
# NOT derived from a variable expansion, which means it is on a line with no $ before it.
# A simple structural check: no line in promote.sh starts with a path literal.
hardcoded="$(grep -nE "^[^#'\"]*[^$]['\"]?/[a-z][a-z0-9_-]+-prod/" "$HERE/promote.sh" 2>/dev/null || true)"
[ -z "$hardcoded" ] && ok "promote.sh has no hardcoded prod-path literal" \
    || bad "promote.sh has no hardcoded prod-path literal" "found: $hardcoded"

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
exported="$(env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$HARNESS/spira/watchers" \
    bash -c ". '$HARNESS/spira/conf.sh'; env | grep '^SPIRA_INSTANCE='" 2>/dev/null)"
want "SPIRA_INSTANCE is exported" "SPIRA_INSTANCE=" "$exported"

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
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
