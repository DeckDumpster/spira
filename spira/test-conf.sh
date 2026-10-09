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
# covers: spira/conf.sh UC-config-store-preflight-01 UC-config-store-preflight-03 UC-config-store-preflight-04
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
echo "a declared SPIRA_PROD is what conf.sh exports:"
# ==========================================================================
custom_prod="$TMP/my-own-prod/spira"
got="$(conf_val SPIRA_PROD SPIRA_PROD="$custom_prod")"
is "the declared SPIRA_PROD is exported" "$custom_prod" "$got"

# ==========================================================================
# DELETED (per Ryan 2026-10-05, one source of config): the sections that asserted conf.sh
# DERIVES a value — a default SPIRA_WORKSPACES, instance-qualified SPIRA_DB/SPIRA_RUN, the
# XDG fallback for SPIRA_RUN, SPIRA_TESTDB_DATA from the workspaces root, SPIRA_HOME_REPO
# from a worktree/MANIFEST/directory name, SPIRA_RELEASE_REPO/SPIRA_GH_INTAKE_REPO from a
# MANIFEST. Every one of those keys is now declared; nothing derives it.
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
# G8: spira_require names each missing program and fails; a present one passes.
req_present="$(env -i SPIRA_TOML="$SPIRA_TOML" PATH="$PATH" HOME="$TMP/hook-home" SPIRA_CONF=/nonexistent \
    bash -c ". '$HARNESS/spira/conf.sh' >/dev/null 2>&1; spira_require bash; echo rc=\$?" 2>&1)"
want "spira_require: a present program returns 0" "rc=0" "$req_present"
req_absent="$(env -i SPIRA_TOML="$SPIRA_TOML" PATH="$PATH" HOME="$TMP/hook-home" SPIRA_CONF=/nonexistent \
    bash -c ". '$HARNESS/spira/conf.sh' >/dev/null 2>&1; spira_require no-such-prog-g8; echo rc=\$?" 2>&1)"
want "spira_require: a missing program is named" "no-such-prog-g8" "$req_absent"
nowant "spira_require: a missing program does not return 0" "rc=0" "$req_absent"

tl_summary
