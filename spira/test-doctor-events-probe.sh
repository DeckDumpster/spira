#!/usr/bin/env bash
#
# test-doctor-events-probe.sh — doctor.sh events substrate round-trip probe (db-9dh)
#
# WHAT THIS GUARDS
# ----------------
# db-9dh replaced the dolt-on-PATH proxy in doctor.sh with a write/read round trip:
# write one sentinel event via _bump_write_event, read it back via _counter_events_query,
# FAIL naming the write path when it does not return.
#
# The server-mode events table enforces fk_events_issue (events.issue_id -> issues.id),
# so a sentinel id that matches no bead is refused with Error 1452. The probe must pick
# a real bead id from bd list — a sentinel against a random uuid always fails.
#
# tier: T1
# covers: spira/doctor.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-doctor-events-probe.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Shared fixtures
# ---------------------------------------------------------------------------
BIN="$TMP/bin"
mkdir -p "$BIN"

# systemctl: all units active/enabled so the systemd sections do not FAIL.
cat > "$BIN/systemctl" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)           printf 'active\n'; exit 0 ;;
    *"is-enabled"*)          printf 'enabled\n'; exit 0 ;;
    *"list-unit-files"*)     true ;;
    *"list-units"*)          true ;;
    *"list-timers"*)         true ;;
esac
exit 0
FAKESCRIPT
chmod +x "$BIN/systemctl"

_SAFE_PATH="$BIN:$PATH"

# ==========================================================================
echo
echo "server mode — the events table enforces fk_events_issue:"
# ==========================================================================
# THE GAP THAT LET THE DEFECT SHIP. A server-mode store carries fk_events_issue
# (events.issue_id -> issues.id), so a sentinel id that matches no bead is REFUSED
# with Error 1452 and the probe can never pass. It reported "writes ... are discarded"
# on every run of every correctly-working server-mode box. A positive control that
# cannot pass is worse than no control: it is a permanent red that trains the reader
# to skip the section.
SRV="$TMP/srv"
mkdir -p "$SRV/bin"
srv_state() { printf '%s' "$2" > "$SRV/$1"; }
srv_state reject_all no
srv_state inserted 0
srv_state issues yes

# A server-mode bd: it enforces the foreign key, and it counts what it accepted.
cat > "$SRV/bin/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
S="$(dirname "$0")/.."
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
esac
# The anchor query: which real issues exist.
case "$*" in
    *list*--json*|*--json*list*)
        if [ "$(cat "$S/issues")" = "yes" ]; then printf '[{"id":"sp-real1"}]\n'
        else printf '[]\n'; fi
        exit 0 ;;
esac
_sql=""; _next=0
for a in "$@"; do [ "$_next" = 1 ] && { _sql="$a"; break; }; [ "$a" = "sql" ] && _next=1; done
[ -n "$_sql" ] || exit 0
case "$_sql" in
    INSERT*)
        if [ "$(cat "$S/reject_all")" = "yes" ]; then
            printf "Error: exec error: Error 1452 (HY000): Foreign key violation on fk: \`fk_events_issue\`\n" >&2
            exit 1
        fi
        case "$_sql" in
            *"'sp-real1'"*)
                printf '%s' "$(( $(cat "$S/inserted") + 1 ))" > "$S/inserted"; exit 0 ;;
            *)
                printf "Error: exec error: Error 1452 (HY000): Foreign key violation on fk: \`fk_events_issue\`\n" >&2
                exit 1 ;;
        esac ;;
    SELECT\ COUNT*)
        printf 'COUNT(*)\n--------\n%s\n(1 rows)\n' "$(cat "$S/inserted")"; exit 0 ;;
esac
exit 0
FAKESCRIPT
chmod +x "$SRV/bin/bd"

run_doctor_srv() {
    env -i \
        PATH="$SRV/bin:$_SAFE_PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$SRV/bin" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_BD="$SRV/bin/bd" \
        SPIRA_DB="$SRV/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        bash "$HERE/doctor.sh" 2>/dev/null
}

# A server-mode store: .beads exists, but NO embeddeddolt directory.
rm -rf "$SRV/db"; mkdir -p "$SRV/db/.beads"
touch "$TMP/watchers-empty"
mkdir -p "$TMP/run" "$TMP/home"

srv_state reject_all no; srv_state inserted 0; srv_state issues yes
srv_out="$(run_doctor_srv || true)"
srv_events="$(printf '%s\n' "$srv_out" | sed -n '/^events substrate$/,/^$/p')"
# The OK form, not a substring: "events write/read round trip FAILED" contains the
# same words, so a bare substring match would call the broken case a pass.
want   "server mode: round trip OK against a real bead id" \
       "ok    events write/read round trip" "$srv_events"
nowant "server mode: no FAIL"  "FAIL" "$srv_events"

# POSITIVE CONTROL. A store that refuses every write must still be caught — and named
# as REFUSED, not as silently discarded.
srv_state reject_all yes; srv_state inserted 0
rej_out="$(run_doctor_srv || true)"
rej_events="$(printf '%s\n' "$rej_out" | sed -n '/^events substrate$/,/^$/p')"
want "server mode: a refused write FAILs"        "FAIL"     "$rej_events"
want "server mode: and is named as refused"      "refused"  "$rej_events"

# A store with no issues at all cannot be probed this way. That is honest ignorance,
# not a fault: say so rather than manufacture a failure the operator cannot act on.
srv_state reject_all no; srv_state inserted 0; srv_state issues no
empty_out="$(run_doctor_srv || true)"
empty_events="$(printf '%s\n' "$empty_out" | sed -n '/^events substrate$/,/^$/p')"
nowant "server mode: an empty store is not a FAIL" "FAIL" "$empty_events"

echo
tl_summary
