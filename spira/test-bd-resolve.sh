#!/usr/bin/env bash
#
# test-bd-resolve.sh — conf.sh resolves SPIRA_BD deterministically and refuses a mismatch.
#
# WHAT THIS SUITE IS FOR
# ----------------------
# When multiple bd binaries exist on PATH, the one each context picks depends on PATH order,
# which differs between a login shell, a systemd unit and an aeon's confined environment.
# SPIRA_BD is the pin: conf.sh sets it once, deterministically, so every child process
# inherits the same binary. And when the resolved bd's migration count disagrees with the
# database's, conf.sh refuses rather than letting the mismatch propagate silently to every
# bdq call.
#
# POSITIVE CONTROL FIRST. Before asserting the pin works, plant two bd binaries on PATH and
# verify that with the wrong one first and no SPIRA_BD set, sourcing conf.sh fails — which
# is the schema refusal in action. That assertion is what would fail if the exit-on-mismatch
# block were removed (the bug shape of sp-s2zvn).
#
# WHAT IS VERIFIED
# 1. POSITIVE CONTROL: when SPIRA_BD resolves to a binary whose migration count disagrees
#    with the database, conf.sh exits non-zero (refuses).
# 2. conf.sh resolves SPIRA_BD to the first bd on its assembled PATH when neither the
#    environment nor the config file set it.
# 3. An env-set SPIRA_BD survives unchanged through conf.sh (env wins).
# 4. A config-file-set SPIRA_BD is respected (config wins over derived default).
# 5. When SPIRA_BD is set to the matching binary, conf.sh succeeds even with a fake db.
# 6. SPIRA_BD is exported so child processes inherit it.
#
# tier: T1
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
isne() { [ "$2" != "$3" ] && ok "$1" || bad "$1" "wanted NOT [$2] got [$3]"; }

echo "test-bd-resolve.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home"

# Minimal harness tree so conf.sh resolves sensibly without touching real paths.
HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"
ln -s "$HERE/conf.sh" "$HARNESS/spira/conf.sh"
printf '# empty\n' > "$HARNESS/spira/repo-map.example"
printf '# empty\n' > "$HARNESS/spira/watchers"

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
# Returns the value of SPIRA_BD; extra env vars can be appended.
conf_val() {
    local spira_path="${1:-}"; shift || true
    env -i PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_HOME="$HARNESS/spira" \
        SPIRA_REPO="$HARNESS" \
        SPIRA_CONF=/nonexistent \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        SPIRA_DB="$TMP/empty-db" \
        SPIRA_PATH="$spira_path" \
        "$@" \
        bash -c ". '$HARNESS/spira/conf.sh'; printf '%s' \"\${SPIRA_BD:-}\"" 2>/dev/null
}

# Source conf.sh with the fake database (schema check fires); returns exit code.
conf_with_db() {
    local spira_path="${1:-}"; shift || true
    env -i PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_HOME="$HARNESS/spira" \
        SPIRA_REPO="$HARNESS" \
        SPIRA_CONF=/nonexistent \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        SPIRA_DB="$FAKEDB" \
        SPIRA_PATH="$spira_path" \
        "$@" \
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
conf_with_db "$TMP/bin-bad" ; rc=$?
if [ "$rc" -ne 0 ]; then
    ok "schema mismatch causes conf.sh to refuse (exit $rc)"
else
    bad "schema mismatch causes conf.sh to refuse" "conf.sh exited 0 — exit-on-mismatch block is absent"
fi

# With the matching bd configured explicitly, conf.sh succeeds even though bin-bad is on PATH.
if conf_with_db "$TMP/bin-bad" SPIRA_BD="$BD_GOOD"; then
    ok "with matching SPIRA_BD set, conf.sh succeeds despite mismatched binary on PATH"
else
    bad "with matching SPIRA_BD set, conf.sh succeeds" "conf.sh exited non-zero unexpectedly"
fi

# ==========================================================================
echo
echo "PATH resolution — SPIRA_BD defaults to first bd on the harness PATH:"
# ==========================================================================
# bin-good is on SPIRA_PATH; no SPIRA_BD in env.
got="$(conf_val "$TMP/bin-good")"
is "SPIRA_BD resolves to first bd on SPIRA_PATH" "$BD_GOOD" "$got"

# bin-bad is first, bin-good is second. Without SPIRA_BD set, resolves to bin-bad.
got="$(conf_val "$TMP/bin-bad:$TMP/bin-good")"
is "SPIRA_BD resolves to the PATH-first binary when unset" "$BD_BAD" "$got"

# ==========================================================================
echo
echo "env wins — SPIRA_BD set in environment is preserved by conf.sh:"
# ==========================================================================
# bin-bad is first on PATH; env explicitly pins bin-good. conf.sh must keep bin-good.
got="$(conf_val "$TMP/bin-bad" SPIRA_BD="$BD_GOOD")"
is "env-set SPIRA_BD survives unchanged (env wins over PATH-first)" "$BD_GOOD" "$got"

# ==========================================================================
echo
echo "config file — SPIRA_BD from spira.conf wins over PATH-derived default:"
# ==========================================================================
CONF_FILE="$TMP/spira.conf"
printf 'SPIRA_BD = %s\n' "$BD_GOOD" > "$CONF_FILE"
# bin-bad is first on PATH; config pins bin-good.
got="$(env -i PATH="/usr/local/bin:/usr/bin:/bin" \
    HOME="$TMP/home" \
    SPIRA_HOME="$HARNESS/spira" \
    SPIRA_REPO="$HARNESS" \
    SPIRA_CONF="$CONF_FILE" \
    SPIRA_WATCHERS="$HARNESS/spira/watchers" \
    SPIRA_DB="$TMP/empty-db" \
    SPIRA_PATH="$TMP/bin-bad" \
    bash -c ". '$HARNESS/spira/conf.sh'; printf '%s' \"\${SPIRA_BD:-}\"" 2>/dev/null)"
is "config-file SPIRA_BD wins over PATH-derived default" "$BD_GOOD" "$got"

# env still overrides the config file.
got="$(env -i PATH="/usr/local/bin:/usr/bin:/bin" \
    HOME="$TMP/home" \
    SPIRA_HOME="$HARNESS/spira" \
    SPIRA_REPO="$HARNESS" \
    SPIRA_CONF="$CONF_FILE" \
    SPIRA_WATCHERS="$HARNESS/spira/watchers" \
    SPIRA_DB="$TMP/empty-db" \
    SPIRA_PATH="$TMP/bin-bad" \
    SPIRA_BD="$BD_BAD" \
    bash -c ". '$HARNESS/spira/conf.sh'; printf '%s' \"\${SPIRA_BD:-}\"" 2>/dev/null)"
is "env wins over config file for SPIRA_BD" "$BD_BAD" "$got"

# ==========================================================================
echo
echo "SPIRA_BD is exported — child processes inherit it:"
# ==========================================================================
exported="$(env -i PATH="/usr/local/bin:/usr/bin:/bin" \
    HOME="$TMP/home" \
    SPIRA_HOME="$HARNESS/spira" \
    SPIRA_REPO="$HARNESS" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$HARNESS/spira/watchers" \
    SPIRA_DB="$TMP/empty-db" \
    SPIRA_PATH="$TMP/bin-good" \
    bash -c ". '$HARNESS/spira/conf.sh'; env | grep '^SPIRA_BD='" 2>/dev/null)"
want "SPIRA_BD appears in the exported environment" "SPIRA_BD=" "$exported"

# ==========================================================================
echo
tl_summary
