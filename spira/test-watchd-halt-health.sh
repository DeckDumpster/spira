#!/usr/bin/env bash
#
# test-watchd-halt-health.sh — inactive daemon shows HALTED during a deliberate world halt
# rather than DEGRADED; notify skips escalation for the same reason.
#
# THE BUG. watchd.sh classified any inactive daemon unit as DEGRADED without consulting the
# halt stamp, so `world.sh stop --hard` was indistinguishable from a watcher that died.
# After sp-wr2n4 wired notify to escalate on DEGRADED, a deliberate halt started paging the
# operator for every stopped watcher once SPIRA_NOTIFY_AGE elapsed.
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL: without a halt stamp an inactive daemon registers DEGRADED and the
#    DEGRADED block names it — proves the test could have caught the bug.
# 2. WITH HALT STAMP: the same inactive daemon registers HALTED and is absent from the
#    DEGRADED block — the two conditions are now distinguishable.
# 3. NOTIFY POSITIVE CONTROL: without a halt stamp, an aged unhealthy file triggers
#    escalation — proves the notify path is reachable.
# 4. NOTIFY WITH HALT STAMP: no escalation fires and the unhealthy file is cleared,
#    so the clock does not accumulate toward SPIRA_NOTIFY_AGE during a known halt.
#
# systemctl is mocked so no real unit manager is touched.
#
# defect: sp-c6tb
# covers: spira/watchd.sh spira/world.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
WATCHD="$HERE/watchd.sh"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
gone()   { [ ! -f "$2" ] && ok "$1" || bad "$1" "file still present: $2"; }

echo "test-watchd-halt-health.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home"

# ---------------------------------------------------------------------------
# Fixture
# ---------------------------------------------------------------------------
RUN="$TMP/run"; mkdir -p "$RUN"
WDIR="$RUN/watchd"; mkdir -p "$WDIR"
MOCK_BIN="$TMP/bin"; mkdir -p "$MOCK_BIN"

# One daemon watcher. Target is a valid absolute path; never actually launched.
# SPIRA_INSTANCE is pinned to a non-default so unit-name assertions are not
# accidentally testing the operator's installed instance.
MAN="$TMP/watchers"
printf 'sentinel|daemon|/usr/bin/true\n' > "$MAN"

# Mock systemctl: returns inactive state for both status and show queries.
# `is-active` emits one line per unit; `show` emits an Id/ActiveState/NRestarts
# block per unit, which is what _wd_notify_health reads.
cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
shift  # --user
cmd="$1"; shift
case "$cmd" in
    is-active)
        for _u in "$@"; do echo "inactive"; done ;;
    show)
        for _a in "$@"; do
            case "$_a" in
                *.service) printf 'Id=%s\nActiveState=inactive\nNRestarts=0\n\n' "$_a" ;;
            esac
        done ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"

# Escalation stub: records each call so tests can count asks.
ASKS="$TMP/asks.log"; : > "$ASKS"
cat > "$MOCK_BIN/notify.sh" <<'NOTIFY'
#!/usr/bin/env bash
printf '=== ask\n' >> "$NOTIFY_LOG"
NOTIFY
chmod +x "$MOCK_BIN/notify.sh"

MAIL="$TMP/mail"

# Halt stamp path matches what world.sh writes.
STAMP="$RUN/world.halted"

# Unhealthy file for the sentinel watcher.
UF="$WDIR/sentinel.unhealthy"

# Common env shared by both watchd commands.
BASE_ENV=(
    HOME="$TMP/home"
    PATH="$PATH"
    SPIRA_CONF=/nonexistent
    SPIRA_PATH="$MOCK_BIN"
    SPIRA_RUN="$RUN"
    SPIRA_INSTANCE=test
    SPIRA_WATCHERS="$MAN"
    SPIRA_MAIL="$MAIL"
)

run_status() {
    env -i "${BASE_ENV[@]}" bash "$WATCHD" status 2>/dev/null
}

run_notify() {
    env -i "${BASE_ENV[@]}" \
        SPIRA_NOTIFY_AGE=0 \
        SPIRA_NOTIFY="$MOCK_BIN/notify.sh" \
        NOTIFY_LOG="$ASKS" \
        SPIRA_ACTIONABLE=WAKEME \
        bash "$WATCHD" notify 2>/dev/null
}

# An escalation is a message in the operator's mailbox; SPIRA_NOTIFY was retired for mail.sh.
asks()      { find "$MAIL/operator/new" -type f 2>/dev/null | wc -l | tr -d ' '; }
reset_run() { rm -rf "$MAIL"; rm -f "$WDIR/"*.unhealthy "$WDIR/"notify-health.escalated 2>/dev/null; }
# Backdate a file by one hour so age > SPIRA_NOTIFY_AGE=0 on the first pass.
backdate()  { printf '%s\n' "$(( $(date +%s) - 3600 ))" > "$1"; }

# ---------------------------------------------------------------------------
echo
echo "1. POSITIVE CONTROL — no halt stamp, inactive daemon:"
rm -f "$STAMP"
out="$(run_status)"
want   "DEGRADED appears in status output"     "DEGRADED" "$out"
nowant "HALTED absent without halt stamp"       "HALTED"   "$out"

# ---------------------------------------------------------------------------
echo
echo "2. HALTED world — inactive daemon reads HALTED, not DEGRADED:"
{ date -u '+%Y-%m-%dT%H:%M:%SZ'; printf 'why: test fixture\n'; } > "$STAMP"
out="$(run_status)"
want   "HALTED appears in status output"        "HALTED"   "$out"
nowant "DEGRADED absent with halt stamp"        "DEGRADED" "$out"

# ---------------------------------------------------------------------------
echo
echo "3. NOTIFY positive control — no halt stamp escalates an aged unhealthy watcher:"
rm -f "$STAMP"
reset_run
backdate "$UF"
run_notify || true
is "escalation fires without halt stamp"        "1"        "$(asks)"

# ---------------------------------------------------------------------------
echo
echo "4. NOTIFY with halt stamp — no escalation, unhealthy file cleared:"
{ date -u '+%Y-%m-%dT%H:%M:%SZ'; printf 'why: test fixture\n'; } > "$STAMP"
reset_run
backdate "$UF"
run_notify || true
is "no escalation with halt stamp"              "0"        "$(asks)"
gone "unhealthy file cleared on halt"           "$UF"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
