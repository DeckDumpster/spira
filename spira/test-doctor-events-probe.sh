#!/usr/bin/env bash
#
# test-doctor-events-probe.sh — doctor.sh events substrate round-trip probe (db-9dh)
#
# WHAT THIS GUARDS
# ----------------
# db-9dh replaced the dolt-on-PATH proxy in doctor.sh with a write/read round trip:
# write one sentinel event via _bump_write_event, read it back via _counter_events_query,
# FAIL naming the write path when it does not return. Without this, seven consecutive
# Maechen passes reported census=0; every call to bump_requeue/recur/reclaim had been
# silently discarded and the dolt check reported OK.
#
# Two properties are tested:
#
#   1. POSITIVE CONTROL. An embedded store where events.log is unwritable (chmod 0444)
#      must FAIL, naming the file-fallback write path. Without this the fix has nothing
#      to confirm — a probe that silently passes a broken write path is no probe at all.
#
#   2. HAPPY PATH. An embedded store with no dolt and a writable SPIRA_DB must report
#      "events write/read round trip" as OK (the file fallback introduced by db-wx4 works).
#
# The positive control was run against the unfixed tree (lines 64-72 before db-9dh):
# doctor.sh printed no "events" line at all — the probe was never performed.
#
# covers: spira/doctor.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in output"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in output" ;; *) ok "$1"; esac; }

echo "test-doctor-events-probe.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Shared fixtures
# ---------------------------------------------------------------------------
BIN="$TMP/bin"
mkdir -p "$BIN"

# bd: refuse "sql" (simulates the CGO_ENABLED=0 embedded binary), pass "list" and
# "migrate schema" so the database and schema sections of doctor.sh do not FAIL.
cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
for arg in "$@"; do
    if [ "$arg" = "sql" ]; then
        printf "Error: 'bd sql' is not yet supported in embedded mode\n" >&2
        exit 1
    fi
done
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*)           printf '[]\n'; exit 0 ;;
    *)                  exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

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

# Build a PATH without dolt so both test cases run in the file-fallback path.
_SAFE_PATH="$BIN"
IFS=: read -ra _pathdirs <<< "${PATH:-/usr/local/bin:/usr/bin:/bin}"
for _d in "${_pathdirs[@]}"; do
    [ -n "$_d" ] || continue
    [ -x "$_d/dolt" ] && continue
    _SAFE_PATH="${_SAFE_PATH}:${_d}"
done

touch "$TMP/watchers-empty"

# run_doctor <extra_env_assignments...>
# Runs doctor.sh under env -i so no ambient spira.conf can sneak in.
run_doctor() {
    local extra="${1:-}"
    env -i \
        PATH="$_SAFE_PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_BD="$BIN/bd" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        ${extra} \
        bash "$HERE/doctor.sh" 2>/dev/null
}

# ==========================================================================
echo
echo "positive control — unwritable events.log must FAIL:"
# ==========================================================================
# Create the embedded store directories, seed events.log as unwritable so
# _bump_write_event's file-fallback silently discards the write, and the
# read-back returns 0, triggering the FAIL path.
mkdir -p "$TMP/db/.beads/embeddeddolt" "$TMP/run" "$TMP/home"
touch "$TMP/db/events.log"
chmod 0444 "$TMP/db/events.log"

ctrl_out="$(run_doctor || true)"
# READ THE EVENTS SECTION, NOT THE WHOLE REPORT. Searching all of doctor's output for
# "FAIL" let an unrelated section's failure satisfy this control, so the probe could stop
# working without the control noticing — which is the exact fault this suite exists to
# catch, one level up.
ctrl_events="$(printf '%s\n' "$ctrl_out" | sed -n '/^events substrate$/,/^$/p')"
# Confirm doctor.sh mentioned events at all — required before trusting the passing case.
want "positive control: events section appears" "events substrate" "$ctrl_out"
# The probe must FAIL: no write path can accept the event.
want "positive control: FAIL line appears"      "FAIL"             "$ctrl_events"
# The failure message must name the write path it actually tried, so the operator knows
# where to look. NOT specifically events.log: which of the two embedded paths is named
# depends on whether the dolt CLI is on PATH, and this fixture cannot guarantee it is not
# — conf.sh rebuilds PATH from SPIRA_PATH, so a dolt installed in the image reappears
# after the fixture has carefully excluded it. Both correct answers name the store.
want "positive control: the attempted write path is named" "$TMP/db" "$ctrl_events"

# Restore write permission for cleanup and subsequent case.
chmod 0644 "$TMP/db/events.log"

# ==========================================================================
echo
echo "happy path — file fallback, no dolt: events round trip must be OK:"
# ==========================================================================
rm -rf "$TMP/db"
mkdir -p "$TMP/db/.beads/embeddeddolt" "$TMP/run" "$TMP/home"
# No events.log pre-created; _bump_write_event creates it on first write.

happy_out="$(run_doctor || true)"

want   "happy path: events round trip OK"  "events write/read round trip"  "$happy_out"
nowant "happy path: no FAIL for events"    "FAIL" \
       "$(printf '%s\n' "$happy_out" | grep -i 'events' || true)"
# The section header must appear regardless of outcome.
want   "happy path: section header present" "events substrate" "$happy_out"

# ==========================================================================
echo
echo "server mode — the events table enforces fk_events_issue:"
# ==========================================================================
# THE GAP THAT LET THE DEFECT SHIP. Every case above is an embedded store using the
# events.log fallback, which has no foreign key and accepts any issue_id. A server-mode
# store carries fk_events_issue (events.issue_id -> issues.id), so a sentinel id that
# matches no bead is REFUSED with Error 1452 and the probe can never pass. It reported
# "writes ... are discarded" on every run of every correctly-working server-mode box,
# while real event writes against real bead ids worked the whole time. A positive control
# that cannot pass is worse than no control: it is a permanent red that trains the reader
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

srv_state reject_all no; srv_state inserted 0; srv_state issues yes
srv_out="$(run_doctor_srv || true)"
srv_events="$(printf '%s\n' "$srv_out" | sed -n '/^events substrate$/,/^$/p')"
# The OK form, not a substring: "events write/read round trip FAILED" contains the
# same words, so a bare substring match would call the broken case a pass.
want   "server mode: round trip OK against a real bead id" \
       "ok    events write/read round trip" "$srv_events"
nowant "server mode: no FAIL"  "FAIL" "$srv_events"

# POSITIVE CONTROL. A store that refuses every write must still be caught — and named
# as REFUSED, not as silently discarded. The two are different faults with different
# fixes, and calling a rejection a discard sends the reader to look at permissions.
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
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
