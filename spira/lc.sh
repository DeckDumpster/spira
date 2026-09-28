#!/usr/bin/env bash
#
# lc.sh — shell entry points onto the lifecycle machine (spira-lc), for sweepers being cut
# over one at a time (sp-ki12s). Sourced, never executed.
#
# WHY ADDITIVE. spira_lifecycle has no row for a bead until sp-t93ky's classifier runs and
# sp-sa8pn's deploy grants access; until then every call here is CANNOT_TELL (exit 2) by
# construction, not a bug. Callers treat that exactly like "not cut over yet": log and carry
# on with whatever bd-label mechanism still governs dispatch. Only sp-wenrl's reader cutover
# and sp-sa8pn's deploy make these events the thing anything actually reads.
#
# EXIT CODES, passed through from spira-lc: 0 applied · 2 cannot tell (row/DB unreachable,
# not yet classified) · 3 refused (illegal transition, terminal state, lost a CAS race).
set -uo pipefail

_lc_json_field() {   # _lc_json_field <json> <python-expr over d>
    printf '%s' "$1" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print(""); raise SystemExit
print(eval(sys.argv[1]))' "$2" 2>/dev/null
}

_lc_json_string() {  # _lc_json_string <raw-text> -> a JSON-quoted string, safe to splice
    python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' "${1:-}" 2>/dev/null
}

# HoldKind's serde tag is its Rust variant name (Poison/Ask/Wait/Operator), because neither
# bead.rs nor spira-lc renames it for serialization — only HoldKind::as_str() (what the
# `holds` column and this library's own lowercase kind names use) is lowercase. This is the
# one place that gap is bridged; every caller of lc_hold/lc_unhold uses the lowercase form.
_lc_hold_kind_tag() {   # _lc_hold_kind_tag <poison|ask|wait|operator> -> Poison|Ask|Wait|Operator
    case "$1" in
        poison)   echo Poison ;;
        ask)      echo Ask ;;
        wait)     echo Wait ;;
        operator) echo Operator ;;
        *)        echo "lc: unknown hold kind '$1'" >&2; return 1 ;;
    esac
}

# lc_show <bead-id> -> the `spira-lc show` JSON on stdout; rc 0 found, 1 no such row, 2
# cannot tell (binary missing, DB unreachable).
lc_show() {
    local id="${1:?lc_show needs a bead id}"
    [ -x "${SPIRA_LC_BIN:-}" ] || { printf '{}'; return 2; }
    "$SPIRA_LC_BIN" show "$id" 2>/dev/null
}

# lc_event <bead-id> <expect-state> <version> <actor> <kind-json> -> applies one event.
# rc 0 applied · 2 cannot tell · 3 refused. Never touches bd; the bead's lifecycle row is
# fetched by the caller (lc_hold/lc_unhold below), because the expect/version pair must
# come from the SAME read the caller reasoned from, not a second one taken here.
lc_event() {
    local id="${1:?}" expect="${2:?}" version="${3:?}" actor="${4:?}" kind="${5:?}"
    [ -x "${SPIRA_LC_BIN:-}" ] || return 2
    "$SPIRA_LC_BIN" event bead "$id" --expect "$expect" --version "$version" --actor "$actor" --kind "$kind" >/dev/null 2>&1
}

# HoldCause is a closed enum (lifecycle/src/reason.rs), never a caller's own prose — the
# category a hold kind already implies. A caller's free-text `cause` still reaches the row
# (as the event's `detail`, lifecycle/src/bead.rs), just not as the typed field itself.
_lc_hold_kind_cause() {   # _lc_hold_kind_cause <poison|ask|wait|operator> -> HoldCause tag
    case "$1" in
        poison)   echo attempts-exhausted ;;
        ask)      echo operator-question ;;
        wait)     echo unlanded-blocker ;;
        operator) echo manual-hold ;;
        *)        echo "lc: unknown hold kind '$1'" >&2; return 1 ;;
    esac
}

# lc_hold <bead-id> <kind: poison|ask|wait|operator> <cause> [actor] -> best-effort: read
# the row, apply Hold. rc 0 applied · 1 no row (not yet classified) · 2 cannot tell (no
# binary, DB down) · 3 refused (terminal, or lost the race — the caller's bd-side write
# stays authoritative either way, so a refusal here is logged, never fatal to the caller).
lc_hold() {
    local id="${1:?lc_hold needs a bead id}" kind="${2:?lc_hold needs a hold kind}" cause="${3:-}" actor="${4:-sentinel}"
    local js; js="$(lc_show "$id")"; local rc=$?
    [ "$rc" = 0 ] || return "$rc"
    local state version
    state="$(_lc_json_field "$js" 'd.get("bead",{}).get("state","")')"
    version="$(_lc_json_field "$js" 'd.get("bead",{}).get("version","")')"
    [ -n "$state" ] && [ -n "$version" ] || return 1
    local tag; tag="$(_lc_hold_kind_tag "$kind")" || return 2
    local causetag; causetag="$(_lc_hold_kind_cause "$kind")" || return 2
    local detailj; detailj="$(_lc_json_string "$cause")"
    lc_event "$id" "$state" "$version" "$actor" "{\"Hold\":{\"kind\":\"$tag\",\"cause\":\"$causetag\",\"detail\":$detailj}}"
}

# lc_unhold <bead-id> <kind> [actor] -> best-effort: read the row, apply Unhold. Same rc
# contract as lc_hold. Design property this restores: releasing a hold suspends nothing new
# and resurrects nothing — the row's state is whatever it already was.
lc_unhold() {
    local id="${1:?lc_unhold needs a bead id}" kind="${2:?lc_unhold needs a hold kind}" actor="${3:-sentinel}"
    local js; js="$(lc_show "$id")"; local rc=$?
    [ "$rc" = 0 ] || return "$rc"
    local state version
    state="$(_lc_json_field "$js" 'd.get("bead",{}).get("state","")')"
    version="$(_lc_json_field "$js" 'd.get("bead",{}).get("version","")')"
    [ -n "$state" ] && [ -n "$version" ] || return 1
    local tag; tag="$(_lc_hold_kind_tag "$kind")" || return 2
    lc_event "$id" "$state" "$version" "$actor" "{\"Unhold\":{\"kind\":\"$tag\"}}"
}

# lc_release <bead-id> [actor] -> best-effort: read the row, apply Release (Working -> Ready,
# clearing the holder). Same rc contract as lc_hold. A voluntary release — the holder is
# done, not gone; lc_holderdead is the same destination for a holder that is simply no
# longer there.
lc_release() {
    local id="${1:?lc_release needs a bead id}" actor="${2:-sentinel}"
    local js; js="$(lc_show "$id")"; local rc=$?
    [ "$rc" = 0 ] || return "$rc"
    local state version
    state="$(_lc_json_field "$js" 'd.get("bead",{}).get("state","")')"
    version="$(_lc_json_field "$js" 'd.get("bead",{}).get("version","")')"
    [ -n "$state" ] && [ -n "$version" ] || return 1
    lc_event "$id" "$state" "$version" "$actor" '"Release"'
}

# lc_holderdead <bead-id> [actor] -> best-effort: read the row, apply HolderDead (Working ->
# Ready, clearing the holder) — the same destination as lc_release, for the case where the
# holder did not release itself (its process is gone, killed or crashed, not a clean exit).
lc_holderdead() {
    local id="${1:?lc_holderdead needs a bead id}" actor="${2:-sentinel}"
    local js; js="$(lc_show "$id")"; local rc=$?
    [ "$rc" = 0 ] || return "$rc"
    local state version
    state="$(_lc_json_field "$js" 'd.get("bead",{}).get("state","")')"
    version="$(_lc_json_field "$js" 'd.get("bead",{}).get("version","")')"
    [ -n "$state" ] && [ -n "$version" ] || return 1
    lc_event "$id" "$state" "$version" "$actor" '"HolderDead"'
}

# lc_drop <bead-id> <reason> [actor] -> best-effort: read the row, apply Drop. ORTHOGONAL —
# legal from any non-terminal state, so this applies whether or not a Claim/Submit ever
# reached the row (design: "Terminal means terminal", but nothing upstream of terminal is
# out of reach).
#
# DropReason is a closed enum (lifecycle/src/reason.rs): `unwanted` is the operator's own
# policy call, and slay.sh --close (this function's only caller today) is exactly that — an
# operator decided this bead should not be done. The caller's own free-text `<reason>`
# still reaches bd (slay.sh's own note), just not this typed field.
lc_drop() {
    local id="${1:?lc_drop needs a bead id}" reason="${2:?lc_drop needs a reason}" actor="${3:-sentinel}"
    local js; js="$(lc_show "$id")"; local rc=$?
    [ "$rc" = 0 ] || return "$rc"
    local state version
    state="$(_lc_json_field "$js" 'd.get("bead",{}).get("state","")')"
    version="$(_lc_json_field "$js" 'd.get("bead",{}).get("version","")')"
    [ -n "$state" ] && [ -n "$version" ] || return 1
    lc_event "$id" "$state" "$version" "$actor" '{"Drop":{"reason":"unwanted"}}'
}

# lc_returned <bead-id> <reason> [actor] -> best-effort: read the row, apply Returned (the
# delivery-exit event legal from IN_DELIVERY, landing the bead in REWORK) — a batch member
# pulled back out of an open batch for a reason specific to it, not the whole batch.
#
# ReturnedReason is a closed enum (lifecycle/src/reason.rs); queue.sh's eject (this
# function's only caller today) is always the queue mode's own `batch-ejected` category.
# The caller's own free-text `<reason>` still reaches land_mark's RED record, just not
# this typed field.
lc_returned() {
    local id="${1:?lc_returned needs a bead id}" reason="${2:?lc_returned needs a reason}" actor="${3:-sentinel}"
    local js; js="$(lc_show "$id")"; local rc=$?
    [ "$rc" = 0 ] || return "$rc"
    local state version
    state="$(_lc_json_field "$js" 'd.get("bead",{}).get("state","")')"
    version="$(_lc_json_field "$js" 'd.get("bead",{}).get("version","")')"
    [ -n "$state" ] && [ -n "$version" ] || return 1
    lc_event "$id" "$state" "$version" "$actor" '{"Returned":{"reason":"batch-ejected"}}'
}

# lc_holds <bead-id> -> the row's current hold kinds, one per line (empty if none, not
# yet classified, or the binary/DB is unreachable — a caller that only wants to know
# whether a specific kind is held should grep this, not treat an empty result as an error).
#
# `holds` is a JSON column; `spira-lc show` hands it back verbatim from `dolt sql -r json`,
# which string-encodes every column — so the value here is JSON text to parse a second
# time, not already a list (rows.rs's own `json_col` does the same double-parse).
lc_holds() {
    local id="${1:?lc_holds needs a bead id}"
    local js; js="$(lc_show "$id")"
    [ $? = 0 ] || return 0
    _lc_json_field "$js" '"\n".join(json.loads(d.get("bead",{}).get("holds") or "[]"))' 2>/dev/null
}

# lc_held <bead-id> <kind> -> 0 if the row currently carries that hold, 1 otherwise
# (including "no such row" and "cannot tell" — a caller that only wants a yes/no answer
# should not have to distinguish those from "not held").
lc_held() {
    local id="${1:?lc_held needs a bead id}" kind="${2:?lc_held needs a hold kind}"
    lc_holds "$id" | grep -qx "$kind"
}

# lc_list_held <kind> -> every bead id currently carrying that hold, one per line (empty if
# none, or the binary/DB is unreachable). The bulk counterpart to lc_held: a sweep over many
# beads (CHECK 4's stale-poison scan) asks this once instead of lc_holds per bead.
lc_list_held() {
    local kind="${1:?lc_list_held needs a hold kind}"
    [ -x "${SPIRA_LC_BIN:-}" ] || return 0
    local js; js="$("$SPIRA_LC_BIN" list --hold "$kind" 2>/dev/null)" || return 0
    _lc_json_field "$js" '"\n".join(r.get("bead_id","") for r in (d if isinstance(d, list) else []))' 2>/dev/null
}

# lc_list_state <state> -> "<id>\t<lease_until>\t<holds-comma-separated>" for every bead
# currently in that state, one per line (empty if none, or the binary/DB is unreachable).
# CHECK 2's reclaim scan asks this once (state=WORKING) instead of a per-bead lc_show call
# against every dispatchable bead — the same bulk-over-per-bead shape lc_list_held gives
# CHECK 4.
lc_list_state() {
    local state="${1:?lc_list_state needs a bead state}"
    [ -x "${SPIRA_LC_BIN:-}" ] || return 0
    local js; js="$("$SPIRA_LC_BIN" list --state "$state" 2>/dev/null)" || return 0
    _lc_json_field "$js" '"\n".join("%s\t%s\t%s" % (
        r.get("bead_id",""),
        r.get("lease_until") if r.get("lease_until") is not None else "",
        ",".join(json.loads(r.get("holds") or "[]"))
    ) for r in (d if isinstance(d, list) else []))' 2>/dev/null
}

# lc_list_all -> "<id>\t<state>\t<holder>" for every bead row spira_lifecycle holds, one per
# line (empty if none, or the binary/DB is unreachable). CHECK 2c's consistency sweep asks
# this once instead of a per-bead lc_show call against every dispatchable bead.
lc_list_all() {
    [ -x "${SPIRA_LC_BIN:-}" ] || return 0
    local js; js="$("$SPIRA_LC_BIN" list 2>/dev/null)" || return 0
    _lc_json_field "$js" '"\n".join("%s\t%s\t%s" % (
        r.get("bead_id",""), r.get("state",""), r.get("holder") or ""
    ) for r in (d if isinstance(d, list) else []))' 2>/dev/null
}

# lc_release <bead-id> [actor] -> best-effort: WORKING -> READY, the holder handing its own
# claim back voluntarily. Same rc contract as lc_hold (0 applied, 1 no row, 2 cannot tell, 3
# refused — a stale or already-vacated row is logged, never fatal to the caller).
lc_release() {
    local id="${1:?lc_release needs a bead id}" actor="${2:-sentinel}"
    local js; js="$(lc_show "$id")"; local rc=$?
    [ "$rc" = 0 ] || return "$rc"
    local state version
    state="$(_lc_json_field "$js" 'd.get("bead",{}).get("state","")')"
    version="$(_lc_json_field "$js" 'd.get("bead",{}).get("version","")')"
    [ -n "$state" ] && [ -n "$version" ] || return 1
    lc_event "$id" "$state" "$version" "$actor" '"Release"'
}

# lc_holder_dead <bead-id> [actor] -> best-effort: WORKING -> READY, the same transition as
# lc_release but with a distinct event name — evidence that nobody released the claim, the
# lease simply outlived its holder (CHECK 2's time-based reap, CHECK 2b's /proc ghost check).
# Same rc contract as lc_hold.
lc_holder_dead() {
    local id="${1:?lc_holder_dead needs a bead id}" actor="${2:-sentinel}"
    local js; js="$(lc_show "$id")"; local rc=$?
    [ "$rc" = 0 ] || return "$rc"
    local state version
    state="$(_lc_json_field "$js" 'd.get("bead",{}).get("state","")')"
    version="$(_lc_json_field "$js" 'd.get("bead",{}).get("version","")')"
    [ -n "$state" ] && [ -n "$version" ] || return 1
    lc_event "$id" "$state" "$version" "$actor" '"HolderDead"'
}
