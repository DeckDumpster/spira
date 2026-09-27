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
    local causej; causej="$(_lc_json_string "$cause")"
    lc_event "$id" "$state" "$version" "$actor" "{\"Hold\":{\"kind\":\"$kind\",\"cause\":$causej}}"
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
    lc_event "$id" "$state" "$version" "$actor" "{\"Unhold\":{\"kind\":\"$kind\"}}"
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
