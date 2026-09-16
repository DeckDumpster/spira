#!/usr/bin/env bash
#
# test-bd-schema-stamp.sh — conf.sh caches the bd schema check keyed on the binary's
#   mtime+size; N sources make zero bd calls when nothing has changed; swapping the
#   binary invalidates the stamp and the mismatch is caught.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the first source
#   calls bd and increments the counter. Only then does an unchanged counter on the
#   second source mean the cache worked — not that counting itself is broken.
#
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
iseq()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-bd-schema-stamp.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"
ln -s "$HERE/conf.sh" "$HARNESS/spira/conf.sh"
printf '# empty\n' > "$HARNESS/spira/watchers"

TESTDB="$TMP/testdb"
mkdir -p "$TESTDB/.beads"

SPIRA_RUN_DIR="$TMP/run"
mkdir -p "$SPIRA_RUN_DIR"

CALL_LOG="$TMP/bd-calls"

# Counting bd stub that exits 0 (schema OK).
make_ok_bd() {
    mkdir -p "$TMP/bin"
    cat > "$TMP/bin/bd" <<STUB
#!/bin/sh
COUNT=\$(cat "$CALL_LOG" 2>/dev/null || printf '0')
printf '%d\n' "\$((COUNT+1))" > "$CALL_LOG"
printf '✓ Schema already at v61\n'
exit 0
STUB
    chmod +x "$TMP/bin/bd"
}

# Counting mismatch bd stub — longer content guarantees a different size than ok_bd.
make_mismatch_bd() {
    mkdir -p "$TMP/bin"
    cat > "$TMP/bin/bd" <<STUB
#!/bin/sh
COUNT=\$(cat "$CALL_LOG" 2>/dev/null || printf '0')
printf '%d\n' "\$((COUNT+1))" > "$CALL_LOG"
printf 'database is at v99\nbinary knows up to v53\n' >&2
exit 1
# mismatch variant — extra content guarantees a different file size than ok_bd
STUB
    chmod +x "$TMP/bin/bd"
}

# Source conf.sh in an isolated subprocess, capture combined output and exit status.
source_conf() {
    local out rc=0
    out=$(env -i \
        PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        SPIRA_BD="$TMP/bin/bd" \
        SPIRA_DB="$TESTDB" \
        SPIRA_RUN="$SPIRA_RUN_DIR" \
        SPIRA_BD_LOCK_SLEEP_UNIT=0 \
        bash -c ". '$HARNESS/spira/conf.sh'; printf 'REACHED-PAST-GUARD\n'" 2>&1) || rc=$?
    printf '%s' "$out"
    return "$rc"
}

# ==========================================================================
echo
echo "POSITIVE CONTROL — first source calls bd; stamp is written:"
# ==========================================================================
make_ok_bd
rm -f "$CALL_LOG"

first_out="$(source_conf)"
calls="$(cat "$CALL_LOG" 2>/dev/null || printf '0')"
iseq "positive control: bd called once on first source" "1" "$calls"
want "positive control: conf.sh continues past schema check" "REACHED-PAST-GUARD" "$first_out"
[ -f "$SPIRA_RUN_DIR/bd-schema-stamp" ] \
    && ok "positive control: stamp file written" \
    || bad "positive control: stamp file written" "stamp not found"

# ==========================================================================
echo
echo "cache hit — subsequent sources make zero bd calls:"
# ==========================================================================
second_out="$(source_conf)"
calls2="$(cat "$CALL_LOG" 2>/dev/null || printf '0')"
iseq "cache hit: bd not called on second source" "1" "$calls2"
want "cache hit: conf.sh continues normally" "REACHED-PAST-GUARD" "$second_out"

third_out="$(source_conf)"
calls3="$(cat "$CALL_LOG" 2>/dev/null || printf '0')"
iseq "cache hit: bd not called on third source" "1" "$calls3"
want "cache hit: conf.sh continues on third source" "REACHED-PAST-GUARD" "$third_out"

# ==========================================================================
echo
echo "binary swap — replacing bd invalidates stamp and catches mismatch:"
# ==========================================================================
# make_mismatch_bd writes different content, guaranteeing a different mtime and size.
make_mismatch_bd
swap_out="$(source_conf 2>&1 || true)"
calls4="$(cat "$CALL_LOG" 2>/dev/null || printf '0')"
if [ "${calls4:-0}" -gt 1 ]; then
    ok "binary swap: bd called again after binary replaced"
else
    bad "binary swap: bd called again after binary replaced" "count is ${calls4:-0} (expected >1)"
fi
nowant "binary swap: REACHED-PAST-GUARD absent (mismatch exits conf.sh)" "REACHED-PAST-GUARD" "$swap_out"
want   "binary swap: schema mismatch reported"                            "schema mismatch"    "$swap_out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
