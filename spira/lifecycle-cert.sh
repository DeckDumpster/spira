# lifecycle-cert.sh — certification onto events (sp-vd9dn). Sourced, never executed.
#
# The one place a gate verdict becomes a bead-machine event: Submit (to establish or move the
# tip) and GatePass/GateRed/GateInfra (the verdict itself), through the spira-lc CLI rather
# than a land_mark write. See lifecycle/src/bead.rs for the transition table this plays
# against and design wiki/projects/spira/designs/bead-lifecycle-state-machine-2026-09-26.md.
#
# INERT UNTIL SPIRA_LIFECYCLE_ENFORCE IS ON (sp-gypjk) — the cutover deploy (sp-sa8pn)
# is what turns it on and grants the DB. Until then every call here returns 2 ("cannot tell")
# having touched nothing, so the legacy landstate path this bead is cutting over stays the
# only one in force. This is deliberate, not a fallback to paper over: design's own non-goal
# is "keeping the loop running during cutover" — there is none, the cutover is downtime.
#
# EVERY CALL NAMES THE EXACT TIP IT IS ABOUT. A bead-machine row keyed to the wrong tree is
# worse than no row: it would let a stale verdict answer for a tree that was never judged.
set -u

LC_LOG="${SPIRA_RUN:-/tmp}/lifecycle-cert.log"

# lc_available -> 0 when the lifecycle machine is on (SPIRA_LIFECYCLE_ENFORCE 1/true). spira-lc
# itself is on every release's PATH (sp-gypjk); whether it is consulted is this switch, never
# whether a binary happens to be found.
lc_available() {
    case "${SPIRA_LIFECYCLE_ENFORCE:-0}" in 1|true) return 0 ;; esac
    return 1
}

_lc_log() {   # _lc_log <bead-id> <verb> <detail...>
    printf '%s %s bead=%s %s\n' "$(date +%s)" "$2" "$1" "${3:-}" >> "$LC_LOG" 2>/dev/null || true
}

# _lc_json_escape <string> — backslash and double-quote only. Every value this file puts
# inside a --kind JSON literal is a git sha, a gate-key hash or a slug this harness itself
# generated (never free text from a branch or a person), so this is a defensive minimum, not
# a full JSON encoder.
_lc_json_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

# _lc_show <bead-id> -> the bead row's JSON (spira-lc show), or empty on any failure.
_lc_show() {
    lc_available || return 2
    spira-lc show "$1" 2>/dev/null
}

# _lc_field <json> <dotted-path, e.g. .bead.state> -> the value, or empty. python3, not
# jq: jq is not guaranteed present (SEEN RED in the testenv container, which has python3 but
# no jq at all — every call here read "cannot tell" instead of the row it was just shown).
_lc_field() {
    printf '%s' "$1" | python3 -c '
import json, sys
try:
    v = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for part in sys.argv[1].lstrip(".").split("."):
    if not isinstance(v, dict) or part not in v:
        sys.exit(0)
    v = v[part]
if v is not None:
    print(v)
' "$2" 2>/dev/null
}

# _lc_gate_red_reason <raw-reason> -> one of lifecycle::reason::GateRedReason's six kebab-
# case variants. bead.rs's GateRed carries a closed enum (reason.rs), not the caller's free
# text — gate.sh's own verdict() reasons (branch-red, syntax, beads-data, foreign-harness,
# plus whatever a future caller passes) map onto it here, once, so a reason spira-lc cannot
# parse never turns an otherwise-applicable event into a silent "cannot tell" (SEEN RED:
# "branch-red" is gate.sh's actual reason token and was never a variant name itself).
_lc_gate_red_reason() {
    case "$1" in
        syntax) printf 'syntax' ;;
        beads-data|foreign-harness) printf 'policy-violation' ;;
        no-rebase) printf 'no-rebase' ;;
        timeout) printf 'timeout' ;;
        confine) printf 'confine' ;;
        *) printf 'suites-failed' ;;
    esac
}

# _lc_event <bead-id> <expect-state> <version> <kind-json> -> spira-lc's own exit code
# (0 applied, 3 refused, 2 cannot tell). Actor is the calling script's own name, so an
# event's producer in the log is never guessed after the fact.
_lc_event() {
    local id="$1" expect="$2" version="$3" kind="$4"
    lc_available || return 2
    spira-lc event bead "$id" \
        --expect "$expect" --version "$version" \
        --actor "$(basename "${0:-lifecycle-cert}")" --kind "$kind" >/dev/null 2>&1
}

# _lc_reach_submitted <bead-id> <tip> -> prints "<state> <version>" on stdout once the row
# has been moved as close to SUBMITTED-at-this-tip as an event can take it: WORKING/REWORK
# advance via Submit, and a CERTIFIED row whose tip differs is voided via Submit (the tip
# invariant). Returns 1 if the row is unreachable (no lifecycle row, or spira-lc down); a
# caller reads that as "cannot tell" and gets it from lc_available's own remaining checks.
_lc_reach_submitted() {
    local id="$1" tip="$2" row state version
    row="$(_lc_show "$id")" || return 1
    state="$(_lc_field "$row" '.bead.state')"
    version="$(_lc_field "$row" '.bead.version')"
    [ -n "$state" ] && [ -n "$version" ] || return 1

    if [ "$state" = WORKING ] || [ "$state" = REWORK ]; then
        _lc_event "$id" "$state" "$version" "{\"Submit\":{\"tip\":\"$(_lc_json_escape "$tip")\"}}" || true
        row="$(_lc_show "$id")" || return 1
        state="$(_lc_field "$row" '.bead.state')"
        version="$(_lc_field "$row" '.bead.version')"
    elif [ "$state" = CERTIFIED ]; then
        local cur_tip
        cur_tip="$(_lc_field "$row" '.bead.tip')"
        if [ "$cur_tip" != "$tip" ]; then
            _lc_event "$id" CERTIFIED "$version" "{\"Submit\":{\"tip\":\"$(_lc_json_escape "$tip")\"}}" || true
            row="$(_lc_show "$id")" || return 1
            state="$(_lc_field "$row" '.bead.state')"
            version="$(_lc_field "$row" '.bead.version')"
        fi
    fi
    [ -n "$state" ] && [ -n "$version" ] || return 1
    printf '%s %s\n' "$state" "$version"
}

# lc_resubmit <bead-id> <tip> -> 0 applied, 2 cannot tell, 3 refused (not in an eligible
# state). Records a moved tip with NO verdict — the tip invariant voiding a stale
# certification (a harness rebase, an amend) when nobody is claiming to have gated the new
# tip yet. batch.sh's stale-certification sweep is this function's first caller: a live tip
# that no longer matches the recorded certification and whose gate key does not prove the
# reviewed content is unchanged must not stay CERTIFIED.
lc_resubmit() {
    local id="$1" tip="$2"
    lc_available || return 2
    local row state version
    row="$(_lc_show "$id")" || { _lc_log "$id" cannot-tell "spira-lc show failed"; return 2; }
    state="$(_lc_field "$row" '.bead.state')"
    version="$(_lc_field "$row" '.bead.version')"
    if [ -z "$state" ] || [ -z "$version" ]; then
        _lc_log "$id" cannot-tell "no lifecycle row yet"
        return 2
    fi
    case "$state" in
        WORKING|REWORK|CERTIFIED) ;;
        *) _lc_log "$id" skip "resubmit: state=$state not eligible for tip=$tip"; return 3 ;;
    esac
    if _lc_event "$id" "$state" "$version" "{\"Submit\":{\"tip\":\"$(_lc_json_escape "$tip")\"}}"; then
        _lc_log "$id" applied "resubmit tip=$tip"
        return 0
    fi
    _lc_log "$id" refused "resubmit tip=$tip"
    return 3
}

# lc_certify <bead-id> <tip> <outcome> [detail] -> 0 applied, 2 cannot tell (inert or
# unreachable — the legacy path is still authoritative), 3 refused (a stale view; not an
# error, see lifecycle::Refusal).
#
#   outcome=pass   detail = gate_key
#   outcome=red    detail = reason (mapped onto GateRedReason by _lc_gate_red_reason)
#   outcome=infra  detail ignored
#
# THE TIP INVARIANT (design §3 / this bead) IS APPLIED HERE, NOT ASSUMED. A row the machine
# still shows WORKING/REWORK has never been told this tip exists, and a row shown CERTIFIED
# against a different tip had that certification voided by a later push — both need a Submit
# event before a gate verdict can land on SUBMITTED. This mirrors bead.rs's own Certified+
# Submit arm; it is not a second copy of that rule, only the shell-side move that reaches it.
lc_certify() {
    local id="$1" tip="$2" outcome="$3" detail="${4:-}"
    lc_available || return 2

    local reached state version
    reached="$(_lc_reach_submitted "$id" "$tip")" || { _lc_log "$id" cannot-tell "no lifecycle row yet"; return 2; }
    read -r state version <<< "$reached"

    if [ "$state" != SUBMITTED ]; then
        _lc_log "$id" skip "state=$state tip=$tip outcome=$outcome — not SUBMITTED"
        return 3
    fi

    local kind
    case "$outcome" in
        pass)
            kind="{\"GatePass\":{\"tip\":\"$(_lc_json_escape "$tip")\",\"gate_key\":\"$(_lc_json_escape "$detail")\"}}"
            ;;
        red)
            kind="{\"GateRed\":{\"tip\":\"$(_lc_json_escape "$tip")\",\"reason\":\"$(_lc_gate_red_reason "$detail")\"}}"
            ;;
        infra)
            kind="{\"GateInfra\":{\"tip\":\"$(_lc_json_escape "$tip")\"}}"
            ;;
        *)
            _lc_log "$id" cannot-tell "unknown outcome $outcome"
            return 2
            ;;
    esac

    if _lc_event "$id" SUBMITTED "$version" "$kind"; then
        _lc_log "$id" applied "$outcome tip=$tip"
        return 0
    fi
    _lc_log "$id" refused "$outcome tip=$tip"
    return 3
}
