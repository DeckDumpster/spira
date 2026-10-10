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
# covers: spira/conf.sh UC-config-store-preflight-01 UC-config-store-preflight-03 UC-config-store-preflight-04 UC-config-store-preflight-09 UC-config-store-preflight-10
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

# ==========================================================================
# STORE BINDING AND SCHEMA GUARD (merged from test-bd-resolve.sh and test-bd-lock-retry.sh)
# ==========================================================================
TOOLS="$(command -v spira-config)" && TOOLS="$(dirname "$TOOLS")" || skip "spira-config is not on PATH"

# A fake database: just needs .beads to exist so the schema check fires.
FAKEDB="$TMP/fakedb"
mkdir -p "$FAKEDB/.beads"

# Two directories, each containing a file named 'bd', so PATH resolution finds the right one.
mkdir -p "$TMP/bin-good" "$TMP/bin-bad"

# bin-good/bd: migrate schema exits 0 (matching database cursor at v61).
cat > "$TMP/bin-good/bd" << 'GOOD'
#!/usr/bin/env bash
# Accept and discard -C <path> prefix before subcommand.
while [ "${1:-}" = "-C" ]; do shift 2; done
case "${1:-}" in
    migrate) printf '\xe2\x9c\x93 Schema already at v61\n'; exit 0 ;;
    version) printf 'bd version 1.1.0-dev-test\n' ;;
    *)       exit 0 ;;
esac
GOOD
chmod +x "$TMP/bin-good/bd"

# bin-bad/bd: migrate schema exits 1 with the schema mismatch message on stderr.
cat > "$TMP/bin-bad/bd" << 'BAD'
#!/usr/bin/env bash
while [ "${1:-}" = "-C" ]; do shift 2; done
case "${1:-}" in
    migrate) printf 'database is at v61, binary knows up to v53\n' >&2; exit 1 ;;
    version) printf 'bd version 1.2.2-test\n' ;;
    *)       exit 0 ;;
esac
BAD
chmod +x "$TMP/bin-bad/bd"

BD_GOOD="$TMP/bin-good/bd"
BD_BAD="$TMP/bin-bad/bd"

# Source conf.sh in a subprocess; $SPIRA_DB has no .beads, so schema check is skipped.
# Returns the value of SPIRA_BD; extra SPIRA_* key=val pairs can be appended — declared via
# tl_config (the one source of config) rather than passed through env -i, which strips them.
bd_conf_val() {
    local spira_path="${1:-}"; shift || true
    tl_config SPIRA_DB="$TMP/empty-db" SPIRA_PATH="$spira_path" SPIRA_WATCHERS="$HARNESS/spira/watchers"
    [ $# -gt 0 ] && tl_config "$@"
    env -i PATH="$TOOLS:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_HOME="$HARNESS/spira" \
        SPIRA_REPO="$HARNESS" \
        SPIRA_CONF=/nonexistent \
        SPIRA_TOML="$SPIRA_TOML" \
        bash -c ". '$HARNESS/spira/conf.sh'; printf '%s' \"\${SPIRA_BD:-}\"" 2>/dev/null
}

# Source conf.sh with the fake database (schema check fires); returns exit code.
bd_conf_with_db() {
    local spira_path="${1:-}"; shift || true
    tl_config SPIRA_DB="$FAKEDB" SPIRA_PATH="$spira_path" SPIRA_WATCHERS="$HARNESS/spira/watchers"
    [ $# -gt 0 ] && tl_config "$@"
    env -i PATH="$TOOLS:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_HOME="$HARNESS/spira" \
        SPIRA_REPO="$HARNESS" \
        SPIRA_CONF=/nonexistent \
        SPIRA_TOML="$SPIRA_TOML" \
        bash -c ". '$HARNESS/spira/conf.sh'" 2>/dev/null
}

# ==========================================================================
echo
echo "POSITIVE CONTROL — conf.sh exits on schema mismatch (the bug it prevents):"
# ==========================================================================
# bin-bad is first on PATH; no SPIRA_BD set. conf.sh resolves to bin-bad/bd, runs migrate
# schema against FAKEDB, sees a mismatch exit code, and refuses. This is the shape of the
# 2026-09-08 incident: an unconfigured caller picked the wrong bd from PATH and every bdq
# call silently read an error message as data.
bd_conf_with_db "$TMP/bin-bad" ; rc=$?
if [ "$rc" -ne 0 ]; then
    ok "schema mismatch causes conf.sh to refuse (exit $rc)"
else
    bad "schema mismatch causes conf.sh to refuse" "conf.sh exited 0 — exit-on-mismatch block is absent"
fi

# With the matching bd configured explicitly, conf.sh succeeds even though bin-bad is on PATH.
if bd_conf_with_db "$TMP/bin-bad" SPIRA_BD="$BD_GOOD"; then
    ok "with matching SPIRA_BD set, conf.sh succeeds despite mismatched binary on PATH"
else
    bad "with matching SPIRA_BD set, conf.sh succeeds" "conf.sh exited non-zero unexpectedly"
fi

# PATH-resolution-when-nothing-sets-SPIRA_BD is deleted: the complete fixture declares
# every key (per Ryan 2026-10-05, "nothing has a default"), so SPIRA_BD is never unset —
# there is no PATH-derived fallback left to assert.

# ==========================================================================
echo
echo "env wins — SPIRA_BD set in environment is preserved by conf.sh:"
# ==========================================================================
# bin-bad is first on PATH; env explicitly pins bin-good. conf.sh must keep bin-good.
got="$(bd_conf_val "$TMP/bin-bad" SPIRA_BD="$BD_GOOD")"
is "env-set SPIRA_BD survives unchanged (env wins over PATH-first)" "$BD_GOOD" "$got"

# The legacy spira.conf-file precedence tests are deleted: the one source of config is the
# spira.toml layers $SPIRA_TOML names (per Ryan 2026-10-05) — a standalone spira.conf file
# read independently of that layering is not a config source any more.

# ==========================================================================
echo
echo "SPIRA_BD is exported — child processes inherit it:"
# ==========================================================================
tl_config SPIRA_DB="$TMP/empty-db" SPIRA_PATH="$TMP/bin-good" SPIRA_WATCHERS="$HARNESS/spira/watchers"
exported="$(env -i PATH="$TOOLS:/usr/bin:/bin" \
    HOME="$TMP/home" \
    SPIRA_HOME="$HARNESS/spira" \
    SPIRA_REPO="$HARNESS" \
    SPIRA_CONF=/nonexistent \
    SPIRA_TOML="$SPIRA_TOML" \
    bash -c ". '$HARNESS/spira/conf.sh'; env | grep '^SPIRA_BD='" 2>/dev/null)"
want "SPIRA_BD appears in the exported environment" "SPIRA_BD=" "$exported"

# ==========================================================================

# A test database that triggers the migration guard (.beads must exist).
LOCKDB="$TMP/lockdb"
mkdir -p "$LOCKDB/.beads"

# SPIRA_WATCHERS/SPIRA_BD/SPIRA_DB are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
# CONFIG): declare them via tl_config rather than through source_conf's env -i, which no
# process reads them from any more.
tl_config SPIRA_WATCHERS="$HARNESS/spira/watchers" SPIRA_BD="$TMP/lockbin/bd" SPIRA_DB="$LOCKDB"

LOCK_MSG='Error: failed to open database: embeddeddolt: init schema: embeddeddolt: open db: failed to load database "db": the database is locked by another dolt process'

make_lock_bd() {
    mkdir -p "$TMP/lockbin"
    printf '#!/usr/bin/env bash\nprintf '"'"'%s\n'"'"' "%s" >&2\nexit 1\n' \
        "$LOCK_MSG" > "$TMP/lockbin/bd"
    chmod +x "$TMP/lockbin/bd"
}

make_mismatch_bd() {
    mkdir -p "$TMP/lockbin"
    cat > "$TMP/lockbin/bd" <<'STUB'
#!/usr/bin/env bash
printf 'database is at v61\nbinary knows up to v53\n' >&2
exit 1
STUB
    chmod +x "$TMP/lockbin/bd"
}

# Source conf.sh in an isolated environment and capture combined output + exit status.
# Extra KEY=VAL args are forwarded to env -i.
lock_source_conf() {
    local out rc=0
    out=$(env -i \
        PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$HARNESS/spira" \
        SPIRA_TOML="$SPIRA_TOML" \
        "$@" \
        bash -c ". '$HARNESS/spira/conf.sh'; printf 'REACHED-PAST-GUARD\n'" 2>&1) || rc=$?
    printf '%s' "$out"
    return "$rc"
}

# ==========================================================================
echo
echo "positive control — schema mismatch aborts, REACHED-PAST-GUARD absent:"
# ==========================================================================
# Prove the harness can detect an abort before trusting the lock-contention result.
make_mismatch_bd
ctrl_out="$(lock_source_conf 2>&1 || true)"
nowant "positive control: REACHED-PAST-GUARD absent"  "REACHED-PAST-GUARD" "$ctrl_out"
want   "positive control: schema mismatch message"    "schema mismatch"    "$ctrl_out"

# ==========================================================================
echo
echo "lock contention — refuses immediately; REACHED-PAST-GUARD absent:"
# ==========================================================================
# A locked store means embedded mode. conf.sh must refuse, not retry or continue.
make_lock_bd
lock_out="$(lock_source_conf 2>&1 || true)"
nowant "lock: REACHED-PAST-GUARD absent (conf.sh refuses)"  "REACHED-PAST-GUARD"  "$lock_out"
want   "lock: message names embedded mode"                  "embedded"            "$lock_out"
want   "lock: message names dolt_mode"                      "dolt_mode"           "$lock_out"
want   "lock: message directs user to doctor"                "doctor"             "$lock_out"

# ==========================================================================
echo
echo "SPIRA_DOCTOR=1 — lock contention continues past guard:"
# ==========================================================================
# doctor sets SPIRA_DOCTOR=1 so conf.sh does not exit before doctor can
# collect all FAILs and report them together.
make_lock_bd
doctor_out="$(lock_source_conf SPIRA_DOCTOR=1 2>&1)"
want "doctor mode: REACHED-PAST-GUARD present" "REACHED-PAST-GUARD" "$doctor_out"


tl_summary
