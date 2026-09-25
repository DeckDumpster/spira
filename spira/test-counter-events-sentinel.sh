#!/usr/bin/env bash
#
# test-counter-events-sentinel.sh — a failed bd sql renders '?', never '0', and incident.sh's
# Sin decision reports itself blind instead of silently reading the outage as "never recurred".
#
# GAP G4 (docs/test-plan/ops-detection-remediation.md). _counter_events_query (lib.sh) used
# to print '0' whenever the `bd sql` call underneath it failed — indistinguishable from "zero
# events really happened". recurs_of, its wrapper, fed that straight into incident.sh's Sin
# escalation (incident.sh:308-311 before this fix), so a database outage read as "this has
# never recurred" and SIN_AT could never be crossed while bd sql stayed down. No suite made
# bd sql fail on this specific path before now — census.sh has its own retry-then-surface
# test (test-census-events.sh), but that covers census_events_run_sql, a different function
# (sp-39yd3, superseding sp-b5blz).
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control, law-a-regression-test-must-be-seen-
# to-fail). The same recurrence loop runs twice against incident.sh's real file_one, against
# two stub bd binaries that differ only in whether their `sql` subcommand succeeds. Run
# against the unfixed tree, the failing-sql loop crosses SIN_AT exactly like the working one
# (both count "0 recurrences known" the same way) and pages the operator on a database outage
# with nothing to show — the opposite of the required "blind, never counted" behaviour.
#
# A STUB bd, NOT A REAL ONE. This suite tests what happens when bd sql cannot be reached at
# all; a real Dolt fixture cannot be made to fail its own driver on demand. law-prefer-the-
# real-dependency governs the *shape* of the response (bd's own tabular `sql` output, matched
# byte-for-byte in the stub), not the substrate.
#
# tier: T1
# covers: spira/lib.sh spira/incident.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-counter-events-sentinel.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_CONF=/nonexistent
export SPIRA_RUN="$TMP/run"
export SPIRA_DB="$TMP/db"
mkdir -p "$SPIRA_RUN"

# ======================================================================================
echo
echo "_counter_events_query / recurs_of against a stub bd whose sql call fails:"
# ======================================================================================
FAIL_BD="$TMP/bd-fail"
cat > "$FAIL_BD" <<'STUBEOF'
#!/usr/bin/env bash
for a in "$@"; do
    [ "$a" = sql ] && { printf 'stub: driver unreachable\n' >&2; exit 1; }
done
exit 0
STUBEOF
chmod +x "$FAIL_BD"

# shellcheck disable=SC1090
. "$HERE/lib.sh"

is "_counter_events_query renders '?' when bd sql fails, not '0'" "?" \
   "$(SPIRA_BD="$FAIL_BD" _counter_events_query sp-x1 recurred)"

_qrc=0
SPIRA_BD="$FAIL_BD" _counter_events_query sp-x1 recurred >/dev/null 2>&1 || _qrc=$?
wantrc "_counter_events_query returns non-zero on a failed query" "1" "$_qrc"

is "recurs_of propagates the sentinel rather than folding it to 0" "?" \
   "$(SPIRA_BD="$FAIL_BD" recurs_of sp-x1)"

# ======================================================================================
echo
echo "incident.sh's Sin decision: blind on a failing bd sql, never silently at 0:"
# ======================================================================================
# make_stub_bd <path> <mode: ok|fail> <ref> <id> — a bd stand-in covering every subcommand
# file_one calls on the recurrence path: list (dedup match), label add/list, note, and sql
# (the recurrence counter). Only `sql`'s behaviour differs between the two modes; everything
# else behaves identically so the two runs are comparable.
make_stub_bd() {
    local path="$1" mode="$2" ref="$3" id="$4" cf="$5" lf="$6"
    printf '0' > "$cf"
    : > "$lf"
    cat > "$path" <<STUBEOF
#!/usr/bin/env bash
REF='$ref'
ID='$id'
CF='$cf'
LF='$lf'
MODE='$mode'
args=("\$@")
[ "\${args[0]:-}" = -C ] && args=("\${args[@]:2}")
sub="\${args[0]:-}"
case "\$sub" in
    list)
        printf '[{"id":"%s","external_ref":"%s","status":"open","labels":[]}]\n' "\$ID" "\$REF"
        exit 0 ;;
    label)
        case "\${args[1]:-}" in
            add)  printf '%s\n' "\${args[3]:-}" >> "\$LF" ;;
            list) cat "\$LF" 2>/dev/null ;;
        esac
        exit 0 ;;
    note)
        exit 0 ;;
    sql)
        if [ "\$MODE" = fail ]; then
            printf 'stub: driver unreachable\n' >&2
            exit 1
        fi
        _q="\${args[1]:-}"
        case "\$_q" in
            INSERT*)
                n=\$(( \$(cat "\$CF" 2>/dev/null || echo 0) + 1 ))
                printf '%s' "\$n" > "\$CF"
                exit 0 ;;
            "SELECT COUNT"*)
                printf 'COUNT(*)\n--------\n%s\n(1 rows)\n' "\$(cat "\$CF" 2>/dev/null || echo 0)"
                exit 0 ;;
        esac
        exit 0 ;;
esac
exit 0
STUBEOF
    chmod +x "$path"
}

# file_one lives in incident.sh, which is sourceable (guarded on BASH_SOURCE vs $0).
# shellcheck disable=SC1090
. "$HERE/incident.sh"
SIN_AT=3
SIN_EXEMPT=0
ILOG="$TMP/incident.log"
INCIDENT_CAUSE=test-cause

run_recurrences() {   # run_recurrences <mode> <ref> <id> -> runs SIN_AT+1 filings, prints log
    local mode="$1" ref="$2" id="$3" bd cf lf i
    bd="$TMP/bd-$mode"; cf="$TMP/cf-$mode"; lf="$TMP/lf-$mode"
    make_stub_bd "$bd" "$mode" "$ref" "$id" "$cf" "$lf"
    : > "$ILOG"
    SPIRA_BD="$bd" SPIRA_HOME="$TMP" SPIRA_DB="$TMP/db-$mode"
    export SPIRA_BD SPIRA_HOME SPIRA_DB
    for i in $(seq 1 "$(( SIN_AT + 1 ))"); do
        printf 'payload %s' "$i" > "$TMP/payload-$mode"
        file_one "$ref" "incident title" "$TMP/payload-$mode" >/dev/null
    done
    cat "$ILOG"
}

# mail.sh stub — records whether an escalation was actually sent.
cat > "$TMP/mail.sh" <<'MAILEOF'
#!/usr/bin/env bash
[ "${1:-}" = send ] || exit 0
printf 'SENT\n' >> "$MAIL_LOG"
MAILEOF
chmod +x "$TMP/mail.sh"

# ---- working bd sql: the true positive control -------------------------------------
export MAIL_LOG="$TMP/mail-ok.log"; : > "$MAIL_LOG"
ok_log="$(run_recurrences ok incident:stub-ok sp-ok1)"
want "working bd sql: SIN_AT is reached and escalated once" \
     "is a SIN at $SIN_AT recurrences" "$ok_log"
is "working bd sql: exactly one mail was sent" "1" \
   "$(grep -c SENT "$TMP/mail-ok.log" 2>/dev/null)"
nowant "working bd sql: no 'blind' log line" "blind" "$ok_log"

# ---- failing bd sql: must be blind, not silently zero -------------------------------
export MAIL_LOG="$TMP/mail-fail.log"; : > "$MAIL_LOG"
fail_log="$(run_recurrences fail incident:stub-fail sp-fail1)"
want "failing bd sql: incident.sh says the recurrence count is unknown" \
     "recurrence count unknown" "$fail_log"
want "failing bd sql: incident.sh says Sin is blind" "Sin is blind" "$fail_log"
nowant "failing bd sql: SIN_AT is never reported crossed while blind" \
       "is a SIN at" "$fail_log"
is "failing bd sql: no mail is sent while the count is unknown" "0" \
   "$(grep -c SENT "$TMP/mail-fail.log" 2>/dev/null)"

tl_summary
