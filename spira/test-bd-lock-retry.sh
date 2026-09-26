#!/usr/bin/env bash
#
# test-bd-lock-retry.sh — conf.sh refuses lock-contention errors immediately.
#
# Lock contention means the store is in embedded mode (one exclusive lock, all
# callers serialise). Embedded mode is refused by doctor.sh (sp-a9nr1); conf.sh
# must treat lock contention as a configuration error and exit immediately,
# not retry or continue.
#
# POSITIVE CONTROL: schema mismatch bd stub runs first to prove the harness can
# detect an abort (REACHED-PAST-GUARD absent). Only then does it mean something
# when the lock-contention case also produces no REACHED-PAST-GUARD.
#
# tier: T1
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-bd-lock-retry.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Minimal harness tree so conf.sh resolves SPIRA_HOME correctly.
HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"
ln -s "$HERE/conf.sh" "$HARNESS/spira/conf.sh"
printf '# empty\n' > "$HARNESS/spira/watchers"

# A test database that triggers the migration guard (.beads must exist).
TESTDB="$TMP/testdb"
mkdir -p "$TESTDB/.beads"

LOCK_MSG='Error: failed to open database: embeddeddolt: init schema: embeddeddolt: open db: failed to load database "db": the database is locked by another dolt process'

make_lock_bd() {
    mkdir -p "$TMP/bin"
    printf '#!/usr/bin/env bash\nprintf '"'"'%s\n'"'"' "%s" >&2\nexit 1\n' \
        "$LOCK_MSG" > "$TMP/bin/bd"
    chmod +x "$TMP/bin/bd"
}

make_mismatch_bd() {
    mkdir -p "$TMP/bin"
    cat > "$TMP/bin/bd" <<'STUB'
#!/usr/bin/env bash
printf 'database is at v61\nbinary knows up to v53\n' >&2
exit 1
STUB
    chmod +x "$TMP/bin/bd"
}

# Source conf.sh in an isolated environment and capture combined output + exit status.
# Extra KEY=VAL args are forwarded to env -i.
source_conf() {
    local out rc=0
    out=$(env -i \
        PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        SPIRA_BD="$TMP/bin/bd" \
        SPIRA_DB="$TESTDB" \
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
ctrl_out="$(source_conf 2>&1 || true)"
nowant "positive control: REACHED-PAST-GUARD absent"  "REACHED-PAST-GUARD" "$ctrl_out"
want   "positive control: schema mismatch message"    "schema mismatch"    "$ctrl_out"

# ==========================================================================
echo
echo "lock contention — refuses immediately; REACHED-PAST-GUARD absent:"
# ==========================================================================
# A locked store means embedded mode. conf.sh must refuse, not retry or continue.
make_lock_bd
lock_out="$(source_conf 2>&1 || true)"
nowant "lock: REACHED-PAST-GUARD absent (conf.sh refuses)"  "REACHED-PAST-GUARD"  "$lock_out"
want   "lock: message names embedded mode"                  "embedded"            "$lock_out"
want   "lock: message names dolt_mode"                      "dolt_mode"           "$lock_out"
want   "lock: message directs user to doctor.sh"            "doctor.sh"           "$lock_out"

# ==========================================================================
echo
echo "SPIRA_DOCTOR=1 — lock contention continues past guard:"
# ==========================================================================
# doctor.sh sets SPIRA_DOCTOR=1 so conf.sh does not exit before doctor can
# collect all FAILs and report them together.
make_lock_bd
doctor_out="$(source_conf SPIRA_DOCTOR=1 2>&1)"
want "doctor mode: REACHED-PAST-GUARD present" "REACHED-PAST-GUARD" "$doctor_out"

echo
tl_summary
