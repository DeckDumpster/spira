#!/usr/bin/env bash
# pr-pass-branch.sh — per-branch pr-mode landing, called by landing-pass.
#
# covers: landing-pass spira/landing.sh spira/lib.sh
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
#   7  pull request merged — recorded as delivered (lifecycle delivery machine, sp-n1ilm)
#   8  pull request closed unmerged — recorded as returned (lifecycle delivery machine)
set -uo pipefail

repo="$1" br="$2" id="$3" baseref="$4" name="${5:-}" tip="$6"

SPIRA_HOME="${SPIRA_HOME:?SPIRA_HOME is unset}"
# shellcheck source=/dev/null
. "$SPIRA_HOME/lib.sh"
# shellcheck source=/dev/null
. "$SPIRA_HOME/lc-delivery.sh"

log()  { printf '%s spira: landing-pass %s: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$id" "$*"; }
act()  { log "ACT: $*"; }

SUBMITTED="${SPIRA_RUN}/submitted"

# A branch already sent under pr mode is not re-sent — UNLESS the base has moved out from
# under the pull request (needs_refresh). A failed submission within its cooldown is also
# skipped.
if submitted "$id" "$tip"; then
    case "$(submitted_rec "$id" state)" in
        pr)
            # PR MODE IS EXTERNAL CUSTODY (design §3.1.2): a human or the forge decides
            # merged/closed, and this pass only observes it. Asked whenever the branch has
            # fallen behind its base (needs_refresh would ask anyway) or has never once been
            # asked since it was last pushed (refreshes=0) — a PR that merges while level
            # with its base would otherwise never reach a gh call at all. Once asked, the
            # answer is handed to needs_refresh as a hint so one pass never asks gh twice,
            # and a branch that is neither behind nor unasked is not asked again either.
            _base_fq="$(qualify_base_ref "$baseref" "$repo")"
            _behind=1
            git -C "$repo" merge-base --is-ancestor "$_base_fq" "refs/heads/$br" 2>/dev/null && _behind=0
            _n="$(submitted_rec "$id" refreshes)"
            case "${_n:-}" in ''|*[!0-9]*) _n=0 ;; esac
            _pr_st=""
            if [ "$_behind" = 1 ] || [ "$_n" = 0 ]; then
                _pr_st="$(pr_state "$repo" "$br")" || _pr_st=""
                case "$_pr_st" in
                    MERGED)
                        _merge_sha="$(cd "$repo" && ghq pr view "$br" --json mergeCommit -q .mergeCommit.oid 2>/dev/null)"
                        lc_deliver_pr_merged "$repo" "$id" "$br" "${_merge_sha:-$baseref}"
                        mark_submitted "$id" "$tip" done
                        act "$br's pull request is merged in $name — delivered"
                        exit 7
                        ;;
                    CLOSED)
                        lc_deliver_pr_closed "$id" "pull request closed unmerged"
                        mark_submitted "$id" "$tip" done
                        act "$br's pull request was closed unmerged in $name — returned"
                        exit 8
                        ;;
                esac
            fi
            [ "$_behind" = 0 ] && exit 6
            if ! needs_refresh "$repo" "$name" "$br" "$id" "$baseref" "$tip" "$_pr_st"; then
                exit 6
            fi
            refresh="$PR_REFRESH_N"
            ;;
        stale)
            git -C "$repo" merge-base --is-ancestor "$baseref" "refs/heads/$br" 2>/dev/null \
                || log "$id: $br is behind $baseref and already escalated — leaving it standing"
            exit 6
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
    if [ "${_ls_st:-}" = RED ] && [ "${_ls_tip:-}" = "$tip" ]; then
        log "tip unchanged since last RED mark — skipping duplicate bump"
        exit 3
    fi
    _reopen_note="$(conflict_reopen_note "$repo" "$br" "$baseref" "$name" "${REBASE_CONFLICTS:-}" "landing-pass")"
    _other_beads="$(other_beads_on_conflicts "$repo" "$br" "$baseref" "${REBASE_CONFLICTS:-}")"
    bump_requeue "$id" merge-conflict >/dev/null 2>&1
    _rq_n="$(requeues_of "$id")"
    if [ "${_rq_n:-0}" -ge "${SPIRA_REBASE_ESCALATE_AT:-3}" ]; then
        bead_reopen "$id" rebase-conflict "$_reopen_note"
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
confine_out="$(confine.sh "$id" "$br" "$repo" "$baseref" "" 2>&1)"
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
_cur_st="$(bead_land_status "$id")"
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
