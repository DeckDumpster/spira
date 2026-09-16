#!/usr/bin/env bash
#
# test-doctor-embedded.sh — doctor FAILs when the production store is in embedded mode.
#
#   ./test-doctor-embedded.sh
#
# WHAT THIS TESTS
# ---------------
# 1. POSITIVE CONTROL. A store with dolt_mode=embedded in .beads/metadata.json must
#    trigger a FAIL before the passing case can be trusted (law-a-matcher-reads-code-not-prose).
# 2. SERVER MODE. With dolt_mode=server, no embedded FAIL is emitted.
# 3. CONTENT OF FAIL. The FAIL names the cost (serialised lock) and the remedy (SPIRA_DOLT_DATA).
#
# covers: spira/doctor.sh
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

echo "test-doctor-embedded.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

BIN="$TMP/bin"
mkdir -p "$BIN" "$TMP/db/.beads" "$TMP/run" "$TMP/home"

cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

cat > "$BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)           printf 'active\n' ;;
    *"is-enabled"*)          printf 'enabled\n'; exit 0 ;;
esac
exit 0
MOCK
chmod +x "$BIN/systemctl"

touch "$TMP/watchers-empty"

run_doctor() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        SPIRA_DOLT_DATA="" \
        bash "$HERE/doctor.sh" 2>/dev/null
}

# ==========================================================================
echo
echo "positive control — embedded mode triggers a FAIL:"
# ==========================================================================
# Plant dolt_mode=embedded. If this case does not produce a FAIL, the check
# cannot fire and the passing cases below prove nothing.
printf '{"dolt_mode":"embedded","dolt_database":"db","project_id":"test-ec2t"}\n' \
    > "$TMP/db/.beads/metadata.json"

embedded_out="$(run_doctor || true)"
want "embedded mode triggers a FAIL" "FAIL" \
    "$(printf '%s\n' "$embedded_out" | grep -i 'embedded' || true)"
want "FAIL names the cost: serialised lock" "one lock" "$embedded_out"
want "FAIL names the remedy: SPIRA_DOLT_DATA" "SPIRA_DOLT_DATA" "$embedded_out"

# ==========================================================================
echo
echo "server mode — no embedded FAIL:"
# ==========================================================================
printf '{"dolt_mode":"server","dolt_database":"db","dolt_server_port":3307,"project_id":"test-ec2t"}\n' \
    > "$TMP/db/.beads/metadata.json"

server_out="$(run_doctor || true)"
nowant "server mode: no embedded FAIL line" "embedded" \
    "$(printf '%s\n' "$server_out" | grep 'FAIL' || true)"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
