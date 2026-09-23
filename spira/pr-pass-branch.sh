#!/usr/bin/env bash
# pr-pass-branch.sh — per-branch pr-mode landing, called by landing-pass.
#
# covers: spira/landing-pass spira/landing.sh spira/lib.sh
#
# pr-pass-branch.sh <repo> <branch> <id> <baseref> <repo-name> <tip>
#
# Exits:
#   0  PR opened, refreshed, or already tracked correctly
#   1  push or PR creation failed
#   2  confine violation (bead reopened)
#   3  rebase conflict (bead reopened)
#   4  bead is no longer closed (race with another actor)
#   5  confine inconclusive (deferred to next pass)
#   6  already submitted, no refresh needed (skip)
set -uo pipefail

repo="$1" br="$2" id="$3" baseref="$4" name="${5:-}" tip="$6"

SPIRA_HOME="${SPIRA_HOME:?SPIRA_HOME is unset}"
# shellcheck source=/dev/null
. "$SPIRA_HOME/lib.sh"

log()  { printf '%s spira: landing-pass %s: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$id" "$*"; }
act()  { log "ACT: $*"; }

SUBMITTED="${SPIRA_RUN}/submitted"

# A branch already sent under pr mode is not re-sent — UNLESS the base has moved out from
# under the pull request (needs_refresh). A failed submission within its cooldown is also
# skipped.
if submitted "$id" "$tip"; then
    case "$(submitted_rec "$id" state)" in
        pr)
            if ! needs_refresh "$repo" "$name" "$br" "$id" "$baseref" "$tip"; then
                exit 6
            fi
            refresh="$PR_REFRESH_N"
            ;;
        *)
            exit 6
            ;;
    esac
else
    refresh=0
fi

if ! rebase_branch "$br" "$baseref" "$repo" "$name"; then
    if [ "${REBASE_FAILURE:-}" != conflict ]; then
        log "could not attempt a rebase of $br onto $baseref (${REBASE_FAILURE:-unknown}) — not a conflict, leaving the bead closed"
        [ "${REBASE_FAILURE:-}" = rebase-refused ] && \
            spira_ask_rebase_refused "$id" "$br" "$name" "${REBASE_REFUSED_REASON:-unknown}"
        exit 1
    fi
    if pr_merged "$repo" "$br"; then
        log "$br does not rebase onto $baseref, but its pull request is merged — landed, not stuck"
        exit 0
    fi
    _cur_base_sha="$(git -C "$repo" rev-parse "$baseref" 2>/dev/null)"
    _ls_st=""; _ls_tip=""; _ls_reason=""
    if _ls="$(land_state "$id" 2>/dev/null)"; then
        read -r _ls_st _ls_tip _ _ls_reason <<< "$_ls" || true
    fi
    if [ "${_ls_st:-}" = RED ] && [ "${_ls_tip:-}" = "$tip" ] && \
       [ "${_ls_reason:-}" = "no-rebase@${_cur_base_sha}" ]; then
        log "tip and base unchanged since last RED mark — skipping duplicate bump"
        exit 3
    fi
    _reopen_note="$(conflict_reopen_note "$repo" "$br" "$baseref" "$name" "${REBASE_CONFLICTS:-}" "landing-pass")"
    _other_beads="$(other_beads_on_conflicts "$repo" "$br" "$baseref" "${REBASE_CONFLICTS:-}")"
    bump_requeue "$id" merge-conflict >/dev/null 2>&1
    _rq_n="$(requeues_of "$id")"
    if [ "${_rq_n:-0}" -ge "${SPIRA_REBASE_ESCALATE_AT:-3}" ]; then
        spira_ask_rebase_loop "$id" "$br" "$name" "$_rq_n" "${REBASE_CONFLICTS:-unknown}" "$_other_beads"
        log "escalated $id — rebase conflict x${_rq_n} on $br"
    else
        bead_reopen "$id" rebase-conflict "$_reopen_note"
        log "reopened $id — does not rebase onto $baseref"
        spira_event bead.reopened "$id" "reopened $id — $br does not rebase onto $baseref in $name" \
            "conflicts in ${REBASE_CONFLICTS:-unknown}; the next aeon is handed the rebase" || true
    fi
    land_mark "$id" RED "$tip" "no-rebase@${_cur_base_sha}"
    exit 3
fi

# Tip has moved after rebase
tip="$(git -C "$repo" rev-parse "$br" 2>/dev/null)"

# CONFINEMENT: a spike's branch must be confined to its allowed paths.
confine_out="$("$SPIRA_HOME/confine.sh" "$id" "$br" "$repo" "$baseref" "" 2>&1)"
confine_rc=$?
if [ "$confine_rc" = 1 ]; then
    bead_reopen "$id" confine-fail "Reopened by landing-pass: $confine_out"
    log "spike branch is not confined to its document: $(printf '%s' "$confine_out" | head -1)"
    land_mark "$id" RED "$tip" confine
    exit 2
elif [ "$confine_rc" != 0 ]; then
    log "confine.sh could not evaluate: $(printf '%s' "$confine_out" | head -1)"
    exit 5
fi

# Re-read the bead before landing — it may have been reopened and reclaimed since the scan.
_cur_st="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except: raise SystemExit
d = d if isinstance(d, list) else [d]
print(d[0].get("status", "-") if d else "-")' 2>/dev/null)"
if [ "${_cur_st:-}" != "closed" ]; then
    log "bead is now ${_cur_st:--} (was closed at scan time) — not landing $br"
    exit 4
fi

if land_pr "$repo" "$br" "$id" "$baseref"; then
    mark_submitted "$id" "$tip" pr "$refresh"
    if [ "$refresh" -gt 0 ]; then
        act "refreshed $br onto $baseref in $name — rebased and force-pushed"
    else
        land_mark "$id" REBASED "$tip" "pr-open:$name"
        act "opened a pull request for $br in $name"
    fi
    exit 0
else
    mark_submitted "$id" "$tip" failed "$refresh"
    exit 1
fi
