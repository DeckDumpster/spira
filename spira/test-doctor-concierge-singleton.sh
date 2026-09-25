#!/usr/bin/env bash
#
# test-doctor-concierge-singleton.sh — doctor.sh's doctor_check_concierge_singleton: a second
# live process holding the Remote Control name "concierge" is reported, not missed.
#
#   ./test-doctor-concierge-singleton.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL. A stubbed concierge.sh reporting a stray holder must produce a FAIL
#    naming its pid before the clean case is trusted.
# 2. CLEAN CASE. No stray holder: ok, no FAIL.
# 3. MISSING concierge.sh. The check WARNs rather than silently reporting nothing.
#
# concierge.sh's own _stray-holders scan is exercised directly by test-concierge.sh; this
# suite is only about doctor.sh wiring its output into a FAIL an operator will see.
#
# covers: spira/doctor.sh concierge.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

echo "test-doctor-concierge-singleton.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

BIN="$TMP/bin"
mkdir -p "$BIN" "$TMP/run" "$TMP/home" "$TMP/repo"

cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
exit 0
FAKESCRIPT
chmod +x "$BIN/bd"

# A STUB, NOT THE REAL concierge.sh. The real one needs claude and tmux on PATH to answer
# _stray-holders honestly; what this suite is proving is that doctor.sh turns whatever it
# says into a FAIL an operator sees, not that the scan itself is correct (test-concierge.sh
# owns that).
write_concierge_stub() {
    # $1: what `_stray-holders` should print, one pid per line ("" for none).
    cat > "$TMP/repo/concierge.sh" <<STUB
#!/usr/bin/env bash
case "\${1:-}" in
    _stray-holders) printf '%s' "$1" ;;
esac
STUB
    chmod +x "$TMP/repo/concierge.sh"
}

run_doctor() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_BD="$BIN/bd" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_REPO="$TMP/repo" \
        SPIRA_INSTANCE=prod \
        bash "$HERE/doctor.sh" 2>/dev/null
}
concierge_section() { sed -n '/^concierge$/,/^$/p' <<< "$1"; }

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — a stray holder FAILs, naming its pid:"
# ==========================================================================
write_concierge_stub "424242"
out="$(run_doctor || true)"
sec="$(concierge_section "$out")"
want "positive control: FAIL fires"    "FAIL"   "$sec"
want "positive control: names the pid" "424242" "$sec"

# ==========================================================================
echo
echo "2. CLEAN CASE — no stray holder: ok, no FAIL:"
# ==========================================================================
write_concierge_stub ""
clean_out="$(run_doctor || true)"
clean_sec="$(concierge_section "$clean_out")"
want   "clean: ok line" "held by no more than the managed session" "$clean_sec"
nowant "clean: no FAIL" "FAIL" "$clean_sec"

# ==========================================================================
echo
echo "3. MULTIPLE STRAYS — each pid is named:"
# ==========================================================================
write_concierge_stub "$(printf '%s\n%s' 111 222)"
multi_out="$(run_doctor || true)"
multi_sec="$(concierge_section "$multi_out")"
want "multi: first pid named"  "111" "$multi_sec"
want "multi: second pid named" "222" "$multi_sec"

# ==========================================================================
echo
echo "4. MISSING concierge.sh — WARN, never a silent clean bill of health:"
# ==========================================================================
rm -f "$TMP/repo/concierge.sh"
missing_out="$(run_doctor || true)"
missing_sec="$(concierge_section "$missing_out")"
want   "missing: warns"                    "warn" "$missing_sec"
nowant "missing: does not false-claim ok"  "held by no more than the managed session" "$missing_sec"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
