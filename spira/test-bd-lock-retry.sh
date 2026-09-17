#!/usr/bin/env bash
#
# test-bd-lock-retry.sh — conf.sh retries a transient dolt lock-contention error; when
#   retries are exhausted and the only failure was lock contention, conf.sh continues with
#   a warning (the lock-holder's presence implies schema compatibility). A schema mismatch
#   aborts on the first attempt without retry.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): a schema mismatch bd stub runs
#   FIRST to prove the test harness can detect an abort (REACHED-PAST-GUARD absent). Only
#   then does it mean anything when the lock-only exhaustion case produces REACHED-PAST-GUARD.
#
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

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

# Write a bd stub that always exits 1 with the lock error.
make_lock_bd() {
    mkdir -p "$TMP/bin"
    printf '#!/usr/bin/env bash\nprintf '"'"'%s\n'"'"' "%s" >&2\nexit 1\n' \
        "$LOCK_MSG" > "$TMP/bin/bd"
    chmod +x "$TMP/bin/bd"
}

# Write a counting bd that emits the lock error for the first FAIL_TIMES calls, then exits 0.
make_counting_lock_bd() {
    local fail_times="$1"
    mkdir -p "$TMP/bin"
    rm -f "$TMP/bd-calls"
    cat > "$TMP/bin/bd" <<STUB
#!/usr/bin/env bash
COUNT=\$(cat "$TMP/bd-calls" 2>/dev/null || printf '0')
COUNT=\$((COUNT+1))
printf '%d\n' "\$COUNT" > "$TMP/bd-calls"
if [ "\$COUNT" -le $fail_times ]; then
    printf 'Error: failed to open database: embeddeddolt: init schema: embeddeddolt: open db: failed to load database "db": the database is locked by another dolt process\n' >&2
    exit 1
fi
printf '✓ Schema already at v61\n'
exit 0
STUB
    chmod +x "$TMP/bin/bd"
}

# Write a counting bd that always exits 1 with a schema-mismatch error.
make_counting_mismatch_bd() {
    mkdir -p "$TMP/bin"
    rm -f "$TMP/bd-calls2"
    cat > "$TMP/bin/bd" <<STUB
#!/usr/bin/env bash
COUNT=\$(cat "$TMP/bd-calls2" 2>/dev/null || printf '0')
COUNT=\$((COUNT+1))
printf '%d\n' "\$COUNT" > "$TMP/bd-calls2"
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
        SPIRA_BD_LOCK_SLEEP_UNIT=0 \
        "$@" \
        bash -c ". '$HARNESS/spira/conf.sh'; printf 'REACHED-PAST-GUARD\n'" 2>&1) || rc=$?
    printf '%s' "$out"
    return "$rc"
}

# ==========================================================================
echo
echo "positive control — schema mismatch aborts, REACHED-PAST-GUARD is absent:"
# ==========================================================================
# Proves the test harness can detect an abort: REACHED-PAST-GUARD must be absent
# and the schema mismatch message must appear. Only then does it mean anything that
# the lock-only exhaustion case below produces REACHED-PAST-GUARD.
make_counting_mismatch_bd
ctrl_out="$(source_conf 2>&1 || true)"
nowant "positive control: REACHED-PAST-GUARD absent"  "REACHED-PAST-GUARD" "$ctrl_out"
want   "positive control: schema mismatch message"    "schema mismatch"    "$ctrl_out"

# ==========================================================================
echo
echo "retry — transient lock (fails once, then succeeds) does not abort:"
# ==========================================================================
make_counting_lock_bd 1
retry_out="$(source_conf 2>&1)"
want "retry: REACHED-PAST-GUARD present" "REACHED-PAST-GUARD" "$retry_out"
calls="$(cat "$TMP/bd-calls" 2>/dev/null || printf '0')"
if [ "${calls:-0}" -gt 1 ]; then
    ok "retry: bd was called more than once (called $calls times)"
else
    bad "retry: bd was called more than once" "called ${calls:-0} times"
fi

# ==========================================================================
echo
echo "lock-only exhaustion — all retries locked, conf.sh continues with warning:"
# ==========================================================================
# When every retry fails with lock contention only (no schema mismatch), the process
# holding the lock opened the database successfully — schema is compatible. Exiting
# here would block callers that do not need database access (e.g. skew.sh foreign)
# whenever collect.sh holds the embedded dolt lock for its 180s probes.
make_lock_bd
exhaust_out="$(source_conf 2>&1)"
want   "exhausted: REACHED-PAST-GUARD present (conf.sh continues)"  "REACHED-PAST-GUARD"           "$exhaust_out"
want   "exhausted: lock warning is printed"                          "locked"                        "$exhaust_out"
nowant "exhausted: no 'migrate schema failed'"                       "migrate schema failed"         "$exhaust_out"
nowant "exhausted: no 'schema mismatch'"                             "schema mismatch"               "$exhaust_out"

# ==========================================================================
echo
echo "schema mismatch — real mismatch aborts on the first attempt without retry:"
# ==========================================================================
make_counting_mismatch_bd
mismatch_out="$(source_conf 2>&1 || true)"
nowant "mismatch: REACHED-PAST-GUARD absent"   "REACHED-PAST-GUARD" "$mismatch_out"
want   "mismatch: schema mismatch message"     "schema mismatch"    "$mismatch_out"
calls2="$(cat "$TMP/bd-calls2" 2>/dev/null || printf '0')"
if [ "${calls2:-0}" -eq 1 ]; then
    ok "mismatch: bd called exactly once (not retried)"
else
    bad "mismatch: bd called exactly once" "called ${calls2:-0} times"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
