#!/usr/bin/env bash
# lc-delivery.sh — wires the delivery state machine (lifecycle crate, design §3.1.2,
# sp-n1ilm) into pr-pass-branch.sh and landing.sh's push mode. Sourced by both, so a
# pr-mode merge and a push-mode push are recorded through the one function that asks
# spira-lc, rather than each growing its own notion of "delivered".
#
# Every function here is best-effort and silent on absence: a delivery row exists only
# once the bead machine's `deliver` event has created it, and that machine is a sibling
# cutover-round bead (sp-o7nbr / row 6) not yet landed. "No row" is therefore the ordinary
# case until the round lands together (design §5) — never an error, and never something a
# caller should let interrupt its own legacy behavior.
#
# covers: spira/lc-delivery.sh spira/pr-pass-branch.sh spira/landing.sh lifecycle/* spira-lc/*
set -u

_lc_json_str() {
    local s="${1:-}"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    s="${s//$'\n'/\\n}"
    s="${s//$'\r'/\\r}"
    printf '"%s"' "$s"
}

# lc_delivery_show <id> -> "<state> <version>" on stdout, or empty + non-zero if there is
# no delivery row (spira-lc unreachable, the bead has no row, or its answer did not parse).
lc_delivery_show() {
    local id="$1" out
    out="$("${SPIRA_LC_BIN:?}" show "$id" 2>/dev/null)" || return 1
    python3 -c '
import sys, json
try:
    d = json.loads(sys.stdin.read())
except Exception:
    raise SystemExit(1)
dv = d.get("delivery")
if not dv:
    raise SystemExit(1)
state = dv.get("state", "")
version = dv.get("version", 0)
print(f"{state} {version}")
' <<< "$out"
}

# lc_delivery_event <id> <expect-state> <version> <actor> <kind-json> — exit 0 applied,
# 3 refused, 2 cannot tell. Never treat 2 as a refusal (spira-lc/src/main.rs's own rule):
# a caller that retries "cannot tell" is safe, one that retries a real refusal is not.
lc_delivery_event() {
    local id="$1" expect="$2" version="$3" actor="$4" kind="$5"
    "${SPIRA_LC_BIN:?}" event delivery "$id" --expect "$expect" --version "$version" \
        --actor "$actor" --kind "$kind"
}

# _lc_deliver <id> <want-state> <actor> <kind-json> — the shared guard every wrapper below
# uses: no row, or a row not in the state this exit applies to, is logged and skipped
# rather than forced. A bead's delivery already Exited (a second sighting of the same PR
# merging, or a retried pass) refuses here exactly the way a stale version would.
_lc_deliver() {
    local id="$1" want="$2" actor="$3" kind="$4" rec state version
    rec="$(lc_delivery_show "$id")" || {
        log "lc: no delivery row for $id — not recording $actor's event (inert until the delivery round lands)"
        return 1
    }
    read -r state version <<< "$rec"
    if [ "$state" != "$want" ]; then
        log "lc: $id delivery is $state, not $want — not recording $actor's event"
        return 1
    fi
    lc_delivery_event "$id" "$want" "$version" "$actor" "$kind"
    local rc=$?
    if [ "$rc" = 0 ]; then
        log "lc: $id delivery $want -> applied ($actor)"
    else
        log "lc: $id delivery event refused or unreachable ($actor, exit $rc)"
    fi
    return "$rc"
}

# lc_deliver_pr_merged <repo> <id> <br> <merge-sha> — pr mode's `merged` exit. The proof is
# a merge-tree comparison against the merge commit, never the branch's own ancestry, because
# a squash merge rewrites every SHA the branch carried (design §3.1.2).
lc_deliver_pr_merged() {
    local repo="$1" id="$2" br="$3" merge_sha="$4" proof
    if content_landed "$repo" "$br" "$merge_sha"; then
        proof=merge-tree
    else
        proof=gh-merged
    fi
    _lc_deliver "$id" PR_OPEN pr-pass-branch \
        "$(printf '{"Delivered":{"merge_sha":%s,"proof":%s}}' "$(_lc_json_str "$merge_sha")" "$(_lc_json_str "$proof")")"
}

# lc_deliver_pr_closed <id> <reason> — pr mode's `closed unmerged` exit.
lc_deliver_pr_closed() {
    local id="$1" reason="$2"
    _lc_deliver "$id" PR_OPEN pr-pass-branch \
        "$(printf '{"Returned":{"reason":%s}}' "$(_lc_json_str "$reason")")"
}

# lc_deliver_push_delivered <id> <merge-sha> — push mode's `pushed` exit. The proof is
# ancestry: a push mode delivery never rewrites the commit it landed.
lc_deliver_push_delivered() {
    local id="$1" merge_sha="$2"
    _lc_deliver "$id" PUSHING landing.sh \
        "$(printf '{"Delivered":{"merge_sha":%s,"proof":%s}}' "$(_lc_json_str "$merge_sha")" "$(_lc_json_str ancestry)")"
}

# lc_deliver_push_requeued <id> <tip> — the base moved and the push lost the race with no
# real conflict; not the bead's fault, so it goes back to CERTIFIED (or SUBMITTED if the
# tip changed — the bead machine's tip invariant decides which, not this call).
lc_deliver_push_requeued() {
    local id="$1" tip="$2"
    _lc_deliver "$id" PUSHING landing.sh \
        "$(printf '{"Requeued":{"tip":%s}}' "$(_lc_json_str "$tip")")"
}

# lc_deliver_push_returned <id> <reason> — a genuine conflict with the base: the failure is
# the bead's own, so it goes back to REWORK.
lc_deliver_push_returned() {
    local id="$1" reason="$2"
    _lc_deliver "$id" PUSHING landing.sh \
        "$(printf '{"Returned":{"reason":%s}}' "$(_lc_json_str "$reason")")"
}
