#!/usr/bin/env bash
#
# supersede-duty.sh — adjudicate supersede-requests, as a watchd daemon.
#
#   supersede-duty.sh pass                           one adjudication pass
#   supersede-duty.sh watch [--interval S] [--ticks N]   loop forever (or N times)
#   supersede-duty.sh health                         non-zero when the loop stopped polling
#
# A supersede-request is a manual hold whose latest hold event carries cause
# supersede-request and the successor as its detail. Each one is surfaced (log line and
# Concierge inbox) and decided in the same pass: the successor LANDED supersedes the bead;
# any other successor state lifts the hold, so the bead is never parked on a request nothing
# answers. Either way the bead gets a note naming the successor's state as the evidence.
#
# covers: spira/supersede-duty.sh spira/watchers spira-lc/src/callers.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

HEALTH_FILE="$SPIRA_RUN/watchd/supersede-duty.health"
ACTOR=supersede-duty

_sd_say() {
    printf '%s %s\n' "$(date -u +%FT%TZ)" "$1"
    { mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" \
        && printf '%s [watch:supersede-duty] %s\n' "$(date -u +%FT%TZ)" "$1" >> "$SPIRA_CONCIERGE_INBOX"; } 2>/dev/null
    return 0
}

# Prints the successor id of the bead's standing supersede-request; non-zero when the
# bead's latest operator hold is anything else.
_sd_request_of() {
    timeout 5 spira-lc history "$1" --machine bead 2>/dev/null | python3 -c '
import sys, json
try: rows = json.load(sys.stdin)
except Exception: sys.exit(1)
by = None
for r in rows:
    if str(r.get("applied")) not in ("1", "true"): continue
    try: ev = json.loads(r.get("evidence") or "{}")
    except Exception: continue
    h = ev.get("Hold") if isinstance(ev, dict) else None
    if h and str(h.get("kind")).lower() in ("manual", "operator"):
        by = h.get("detail") if h.get("cause") == "supersede-request" else None
if not by: sys.exit(1)
print(by)'
}

_sd_note() {
    BD_TIMEOUT=5 bdq note "$1" "$2" >/dev/null 2>&1 || _sd_say "NOTE FAILED $1: $2"
}

_sd_settled() { [ -e "$SPIRA_RUN/watchd/supersede-duty.settled/$1" ]; }
_sd_settle() { mkdir -p "$SPIRA_RUN/watchd/supersede-duty.settled" && : > "$SPIRA_RUN/watchd/supersede-duty.settled/$1"; }

# A terminal row is final: only the stale hold is cleared, through the lifecycle, and nothing
# is written to bd. A failure is reported once per bead, then retried quietly.
_sd_retry() {
    _sd_settled "$1.failed" || { _sd_settle "$1.failed"; _sd_say "SUPERSEDE REQUEST $1: $2 — will retry"; }
    return 0
}

_sd_adjudicate() {
    local id="$1" succ="$2" st own
    own="$(timeout 5 spira-lc state "$id" 2>/dev/null)"
    case "$own" in
        LANDED|SUPERSEDED|DROPPED|DONE)
            _sd_settled "$id" && return 0
            timeout 5 spira-lc unhold "$id" manual "$ACTOR" >/dev/null 2>&1
            _sd_settle "$id"
            _sd_say "SUPERSEDE REQUEST $id: already $own; nothing to do"
            return 0 ;;
    esac
    _sd_settled "$id.failed" || _sd_say "SUPERSEDE REQUEST $id: successor $succ"
    st="$(timeout 5 spira-lc state "$succ" 2>/dev/null)"
    if [ "$succ" = "$id" ] || [ -z "$st" ]; then
        timeout 5 spira-lc unhold "$id" manual "$ACTOR" || { _sd_retry "$id" "unhold failed"; return 0; }
        _sd_note "$id" "supersede-request by $succ refused: ${st:-no lifecycle row for $succ}. Hold lifted; the bead proceeds."
        _sd_say "SUPERSEDE REQUEST $id: refused, successor $succ has no usable row; unheld"
    elif [ "$st" = LANDED ]; then
        timeout 5 spira-lc supersede "$id" "$succ" "$ACTOR" || { _sd_retry "$id" "supersede failed"; return 0; }
        timeout 5 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" supersede "$id" --with "$succ" >/dev/null 2>&1 \
            || _sd_say "SUPERSEDE REQUEST $id: lifecycle row SUPERSEDED but bd supersede failed (not retried)"
        _sd_note "$id" "supersede-request confirmed: $succ is LANDED. Superseded by $succ."
        _sd_say "SUPERSEDE REQUEST $id: confirmed, superseded by LANDED $succ"
    else
        timeout 5 spira-lc unhold "$id" manual "$ACTOR" || { _sd_retry "$id" "unhold failed"; return 0; }
        _sd_note "$id" "supersede-request by $succ refused: $succ is $st, not LANDED. Hold lifted; the bead proceeds."
        _sd_say "SUPERSEDE REQUEST $id: refused, $succ is $st; unheld"
    fi
}

cmd_pass() {
    local id succ
    while IFS= read -r id; do
        [ -n "$id" ] || continue
        succ="$(_sd_request_of "$id")" || continue
        _sd_adjudicate "$id" "$succ"
    done < <(timeout 5 spira-lc list-held manual 2>/dev/null)
}

_sd_write_health() {
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$1" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || { echo "supersede-duty: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=60 ;; esac
    age=$(( $(date +%s) - t ))
    [ "$age" -le $(( 3 * iv )) ] || { echo "supersede-duty: last poll ${age}s ago (interval ${iv}s) — hung or dead" >&2; return 1; }
}

cmd_watch() {
    local interval=60 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "supersede-duty.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        cmd_pass
        _sd_write_health "$interval"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        pass)   cmd_pass ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: supersede-duty.sh pass | watch [--interval S] [--ticks N] | health" >&2; exit 2 ;;
    esac
fi
