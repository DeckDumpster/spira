#!/usr/bin/env bash
#
# test-install-prod-checkout.sh — install.sh's _prod_guard and _configure_prod_guard,
# called directly. install.sh defines both before its argument parsing and returns
# without running when sourced (BASH_SOURCE[0] != $0), so this suite never runs doctor.sh,
# never renders a unit, and never touches a real git checkout beyond the one it makes here.
#
#   ./test-install-prod-checkout.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. GIT CHECKOUT: SPIRA_PROD is a git checkout → _prod_guard exits 2, names "git
#    checkout", names the override.
# 2. OVERRIDE: SPIRA_INSTALL_PROD_GIT_CONSIDERED=1 bypasses the guard.
# 3. CLEAN: SPIRA_PROD is not a git checkout → guard does not fire.
# 4. CONFIGURE_PROD WITHOUT conf.sh: _configure_prod_guard exits 1, names "conf.sh",
#    names the "/spira" remedy path.
# 5. CONFIGURE_PROD WITH conf.sh: guard does not fire.
#
# FAIL-FIRST: the git-checkout case is verified first so a silent clean case is
# believed (law-absence-needs-a-positive-control).
#
# tier: T1
# covers: install.sh UC-instance-lifecycle-17
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-install-prod-checkout.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Sourcing install.sh (not executing it) defines _prod_guard and _configure_prod_guard
# and returns before argument parsing, doctor.sh or any phase runs.
. "$HERE/../install.sh"

GIT_PROD="$TMP/git-checkout"
mkdir -p "$GIT_PROD"
git init -q "$GIT_PROD" 2>/dev/null

# ===========================================================================
echo
echo "POSITIVE CONTROL: git checkout at SPIRA_PROD fires the guard"
# ===========================================================================
_git_out="$(_prod_guard "$GIT_PROD" 2>&1)"; _git_rc=$?
wantrc "git-checkout: exits 2"                       "2" "$_git_rc"
want   "git-checkout: names 'git checkout'"          "git checkout" "$_git_out"
want   "git-checkout: names the override"            "SPIRA_INSTALL_PROD_GIT_CONSIDERED" "$_git_out"
want   "git-checkout: names SPIRA_PROD path"         "$GIT_PROD" "$_git_out"

# ===========================================================================
echo
echo "OVERRIDE: SPIRA_INSTALL_PROD_GIT_CONSIDERED=1 bypasses the guard"
# ===========================================================================
_ov_out="$(SPIRA_INSTALL_PROD_GIT_CONSIDERED=1 _prod_guard "$GIT_PROD" 2>&1)"; _ov_rc=$?
wantrc "override: exits 0"                                "0" "$_ov_rc"
nowant "override: does not mention 'is a git checkout'"   "is a git checkout" "$_ov_out"

# ===========================================================================
echo
echo "CLEAN: non-git SPIRA_PROD does not trigger the guard"
# ===========================================================================
CLEAN_PROD="$TMP/clean-prod"
mkdir -p "$CLEAN_PROD"

_clean_out="$(_prod_guard "$CLEAN_PROD" 2>&1)"; _clean_rc=$?
wantrc "clean: exits 0"                                 "0" "$_clean_rc"
nowant "clean: does not mention 'is a git checkout'"    "is a git checkout" "$_clean_out"

# ===========================================================================
echo
echo "CONFIGURE_PROD without conf.sh fires the guard"
# ===========================================================================
NO_CONF_DIR="$TMP/no-conf"
mkdir -p "$NO_CONF_DIR"

_no_conf_out="$(_configure_prod_guard "$NO_CONF_DIR" 2>&1)"; _no_conf_rc=$?
wantrc "CONFIGURE_PROD without conf.sh: exits 1"      "1"            "$_no_conf_rc"
want   "CONFIGURE_PROD without conf.sh: names conf.sh"  "conf.sh"    "$_no_conf_out"
want   "CONFIGURE_PROD without conf.sh: names /spira"   "/spira"     "$_no_conf_out"
want   "CONFIGURE_PROD without conf.sh: names the path" "$NO_CONF_DIR" "$_no_conf_out"

# ===========================================================================
echo
echo "CONFIGURE_PROD with conf.sh does not fire the guard"
# ===========================================================================
WITH_CONF_DIR="$TMP/with-conf"
mkdir -p "$WITH_CONF_DIR"
touch "$WITH_CONF_DIR/conf.sh"

_with_conf_out="$(_configure_prod_guard "$WITH_CONF_DIR" 2>&1)"; _with_conf_rc=$?
wantrc "CONFIGURE_PROD with conf.sh: exits 0"  "0" "$_with_conf_rc"
nowant "CONFIGURE_PROD with conf.sh: does not mention 'does not contain conf.sh'" \
    "does not contain conf.sh" "$_with_conf_out"

tl_summary
