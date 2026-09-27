#!/usr/bin/env bash
# batch.sh — merge-queue pre-cut sweep.
# Usage: batch.sh <repo-name>
#
# The batcher crate (batcher-cut, sp-jzfog) owns the round itself — trigger, membership,
# local proving and the PR — in place of this file's old cut (sp-vsob2). What is left here
# runs every pass regardless of who cuts next: reconciling landstate against what git and bd
# actually say (orphaned CERTIFIED records, closed beads with a live evicted branch, LANDED
# records whose tip never reached base, a stale gate-key, a tip that moved since
# certification), the queue-stuck alert, and the open batch's own DIRTY-PR abandon check.
# queue.sh's _batch_cut runs this before the batcher, unconditionally.
#
# covers: spira/batch.sh spira/conf.sh spira/landing.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"


# format_batch <worktree> <base_sha> [repo-name] -> 0 always; the batch stands
# whatever the formatter does. Same hygiene as format_rebased in lib.sh, applied
# to the tree a batch merge synthesises.
format_batch() {
    local wt="$1" base="$2" name="${3:-}" cmd paths f staged=0

    [ -n "$name" ] || name="$(repo_name_at "$(git -C "$wt" rev-parse --show-toplevel 2>/dev/null)" 2>/dev/null)" || return 0
    cmd="$(repo_format "$name" 2>/dev/null)"
    [ -n "$cmd" ] || return 0

    if ! ( cd "$wt" && env -i PATH="$HOME/.cargo/bin:$PATH" HOME="$HOME" TERM=dumb \
             timeout "${SPIRA_FORMAT_TIMEOUT:-300}" bash -c "$cmd" ) >/dev/null 2>&1; then
        log "format: $name's formatter failed on batch — leaving it unformatted"
        git -C "$wt" checkout -q -- . 2>/dev/null
        return 0
    fi

    paths=()
    while IFS= read -r -d '' f; do
        [ -f "$wt/$f" ] && paths+=("$f")
    done < <(git -C "$wt" diff -z --name-only "$base" HEAD 2>/dev/null)
    [ "${#paths[@]}" -gt 0 ] && git -C "$wt" add -- "${paths[@]}" 2>/dev/null

    git -C "$wt" checkout -q -- . 2>/dev/null
    git -C "$wt" diff --cached --quiet 2>/dev/null || staged=1
    [ "$staged" = 1 ] || return 0

    git -C "$wt" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" commit -q -F - <<EOF 2>/dev/null
spira: format batch

Batch assembly merges branches without re-running $name's formatter, so the
assembled tree may fail the required layout check. Formatted with: $cmd
EOF
    log "format: formatted $name batch"
    return 0
}

# THE PRE-FLIGHT WALL (sp-mb92t). Per Ryan, 2026-09-24: the local pre-flight "CANNOT take
# more than 4 minutes locally; if it does, we need to start evicting tests." Everything the
# pre-flight runs — the gate, attribution, the re-gate — shares one deadline, _PF_DEADLINE.
#
# _pf_run <secs> <cmd...> runs the command in its OWN PROCESS GROUP and kills the whole group
# at the wall: `timeout` alone signals only its child, and would leave the test container it
# started running on. Returns 124 when the wall was hit.
_pf_run() {
    local secs="$1"; shift
    [ "$secs" -gt 0 ] 2>/dev/null || return 124
    local flag; flag="$(mktemp)"; rm -f "$flag"
    setsid "$@" &
    local pid=$!
    # `sleep && …`, never `sleep; …`: cancelling the watchdog kills its sleep, and a killed
    # sleep must END the watchdog — with `;` it would fall through, flag the wall and kill a
    # command that had already finished, turning every fast red into a false timeout.
    ( sleep "$secs" && { : > "$flag"; kill -TERM -"$pid" 2>/dev/null; sleep 15 && kill -KILL -"$pid" 2>/dev/null; } ) \
        </dev/null >/dev/null 2>&1 &
    local dog=$!
    wait "$pid"; local rc=$?
    # CANCEL THE WATCHDOG'S CHILDREN TOO. Its `sleep` inherited every open descriptor —
    # including the queue lock batch.sh holds — so killing only the subshell leaves that
    # sleep holding the lock for the rest of the wall, and every later cut finds the queue
    # "held by another operation".
    pkill -P "$dog" 2>/dev/null; kill "$dog" 2>/dev/null; wait "$dog" 2>/dev/null
    if [ -e "$flag" ]; then rm -f "$flag"; return 124; fi
    return "$rc"
}

_pf_left() {   # seconds left before the pre-flight wall; 0 when spent
    local l=$(( ${_PF_DEADLINE:-0} - $(date +%s) ))
    [ "$l" -gt 0 ] && printf '%s' "$l" || printf '0'
}

# _pf_gate <branch> <name> <stamp> — the batch gate, fast suites only, inside the wall.
_pf_gate() {
    SPIRA_GATE_FAST_MAX_SECS="${SPIRA_PREFLIGHT_SUITE_MAX_SECS:-60}" SPIRA_GATE_BEAD="batch-$3" \
        _pf_run "$(_pf_left)" bash "$HERE/gate.sh" "$1" "$2" 2>&1
}

_batch_open_file() { printf '%s/%s/open' "${SPIRA_QUEUE_DIR:?}" "$1"; }

_batch_is_open() { [ -f "$(_batch_open_file "$1")" ]; }

# _abandon_open_batch <name> <forge> <repo> <ob_file> <pr_n> <members_val> <comment>
# Closes PR, returns innocent members to CERTIFIED, archives the open record.
# Single abandonment path — used by DIRTY and express eviction alike.
_abandon_open_batch() {
    local name="$1" forge="$2" repo="$3" ob_file="$4" pr_n="$5" members_val="$6" comment_text="${7:-Batch abandoned.}"

    # spira-lc's own OPEN-batch lifecycle (sp-o7nbr.2): one cascade returns every member
    # to CERTIFIED (tip unchanged) or SUBMITTED (tip moved) atomically. batch_id/version
    # are present only on a record this session's open_batch wrote (cut succeeded) — a
    # record from before this cutover, or one whose cut refused, has neither, and the new
    # call is skipped rather than CASing against a batch that was never written there.
    local _lc_batch_id _lc_version
    _lc_batch_id="$(grep '^batch_id=' "$ob_file" 2>/dev/null | head -1)"; _lc_batch_id="${_lc_batch_id#batch_id=}"
    _lc_version="$(grep '^version=' "$ob_file" 2>/dev/null | head -1)"; _lc_version="${_lc_version#version=}"
    if [ -n "$_lc_batch_id" ] && [ -n "$_lc_version" ]; then
        local _lc_out _lc_rc
        _lc_out="$(lcq abandon-batch "$_lc_batch_id" --expect OPEN --version "$_lc_version" \
            --actor batch.sh --reason "$comment_text" 2>&1)"
        _lc_rc=$?
        if [ "$_lc_rc" -eq 0 ]; then
            printf 'batch %s: %s abandoned on spira-lc\n' "$name" "$_lc_batch_id"
        else
            printf 'batch %s: spira-lc abandon-batch refused for %s (rc=%d): %s\n' \
                "$name" "$_lc_batch_id" "$_lc_rc" "$_lc_out" >&2
        fi
    fi

    local _m mid mtip cur_state
    for _m in $members_val; do
        mid="${_m%%:*}"; mtip="${_m##*:}"
        cur_state=""
        [ -f "$LANDSTATE/$mid" ] && { read -r cur_state _ < "$LANDSTATE/$mid" 2>/dev/null || true; }
        case "${cur_state:-}" in
        RED|EJECTED)
            printf 'batch %s: %s left at %s\n' "$name" "$mid" "$cur_state" ;;
        *)
            land_mark "$mid" CERTIFIED "$mtip"
            printf 'batch %s: %s returned to CERTIFIED\n' "$name" "$mid" ;;
        esac
    done
    local branch_val
    branch_val="$(grep '^branch=' "$ob_file" 2>/dev/null | head -1)"; branch_val="${branch_val#branch=}"
    queue_cancel_branch_runs "$forge" "$repo" "$branch_val" "QUEUE" || true
    "$forge" pr-comment "$repo" "$pr_n" "$comment_text" 2>/dev/null || true
    "$forge" pr-close   "$repo" "$pr_n" 2>/dev/null || true
    local ob_stamp; ob_stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    mv "$ob_file" "$(dirname "$ob_file")/closed-pr${pr_n}-${ob_stamp}" \
        2>/dev/null || rm -f "$ob_file"
}

# _closed_red_live <repo-path> — print id for each RED/EJECTED landstate whose branch
# exists and whose bead is closed — the eviction-race shape where a bead ends up
# closed+RED+live and no queue mechanism retrieves it.
_closed_red_live() {
    local f id st _crl_st
    [ -d "$LANDSTATE" ] || return 0
    for f in "$LANDSTATE/"*; do
        [ -f "$f" ] || continue
        id="$(basename "$f")"
        case "$id" in .*|*/*) continue ;; esac
        { read -r st _ < "$f"; } 2>/dev/null || continue
        [ "$st" = "RED" ] || [ "$st" = "EJECTED" ] || continue
        git -C "$1" show-ref --verify -q "refs/heads/spira/$id" 2>/dev/null || continue
        _crl_st="$(bdjson show "$id" 2>/dev/null \
            | python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
d=d if isinstance(d,list) else [d]
if d and d[0].get("status")=="closed": print("closed")' 2>/dev/null)" || _crl_st=""
        [ "${_crl_st:-}" = "closed" ] || continue
        printf '%s\n' "$id"
    done
}

# _certified_list <repo-path> — delegates to queue_certified_list in lib.sh.
# Kept as a local alias so callers inside this file do not need updating.
_certified_list() { queue_certified_list "$@"; }

# _base_conflict <repo> <base-sha> <tip>
# 0 when tip conflicts with base alone (branch must be reopened).
# 1 when tip merges cleanly with base (conflict is only with batch accumulation).
# 2 when the test worktree cannot be created (conservative: treat as no-base-conflict).
_base_conflict() {
    local repo="$1" base="$2" tip="$3" wt rc=1
    wt="$SPIRA_RUN/worktree/.batch-ck-$$"
    git -C "$repo" worktree add -q --detach "$wt" "$base" 2>/dev/null || return 2
    git -C "$wt" merge --no-commit --no-ff "$tip" >/dev/null 2>&1 || rc=0
    git -C "$wt" merge --abort 2>/dev/null || true
    git -C "$repo" worktree remove -f "$wt" 2>/dev/null || true
    return $rc
}

# queue_last_moved <landstate-dir> -> the newest epoch among BATCHED/LANDED records,
# or 0 when none exist. land_mark writes without a trailing newline, so `read` hits EOF
# without a delimiter and returns non-zero on the very record this must not skip — every
# read here is checked by content (a parsed epoch), never by read's own exit status.
queue_last_moved() {
    local dir="$1" last=0 f st tip epoch
    [ -d "$dir" ] || { printf '0\n'; return 0; }
    for f in "$dir"/*; do
        [ -f "$f" ] || continue
        st=""; tip=""; epoch=""
        read -r st tip epoch _ < "$f" 2>/dev/null || true
        case "$st" in BATCHED|LANDED) : ;; *) continue ;; esac
        case "${epoch:-}" in ''|*[!0-9]*) continue ;; esac
        [ "$epoch" -gt "$last" ] && last="$epoch"
    done
    printf '%s\n' "$last"
}

# queue_stuck_action <last-moved> <now> <stuck-age-threshold> <flag-exists 0|1> [<stall-from>]
# -> one of:
#   no-history   nothing has ever moved; the check cannot tell stall from a brand-new
#                queue, so it refuses rather than guessing (law-a-control-that-cannot-check-must-refuse)
#   not-stuck    recent movement; clear any stale flag
#   alert        newly stalled; mail once and set the flag
#   already-alerted  still stalled but the flag is already set; stay quiet
# <stall-from>, when given, is the clock the stuck_age is measured from — the caller may
# advance it past <last-moved> (never before it) when nothing was waiting yet at the last
# move, so a deep but draining queue doesn't page on depth (sp-w4tyd). The no-history gate
# still reads <last-moved> itself: a brand-new queue must refuse regardless of that clock.
queue_stuck_action() {
    local last_moved="$1" now="$2" threshold="$3" flag_exists="$4" stall_from="${5:-$1}" stuck_age
    if [ "$last_moved" -eq 0 ]; then printf 'no-history\n'; return 0; fi
    stuck_age=$(( now - stall_from ))
    if [ "$stuck_age" -lt "$threshold" ]; then
        printf 'not-stuck\n'
    elif [ "$flag_exists" = 0 ]; then
        printf 'alert\n'
    else
        printf 'already-alerted\n'
    fi
}

main() {
    local name="${1:-}"
    [ -n "$name" ] || { printf 'batch.sh: repo name required\n' >&2; exit 1; }

    local repo base base_sha remote base_branch mode
    repo="$(repo_root "$name")" || { printf 'batch %s: no repo-map entry\n' "$name" >&2; return 1; }
    mode="$(repo_land "$name")"
    if [ "$mode" != "queue" ]; then
        printf 'batch %s: no cut — mode is %s\n' "$name" "$mode"
        return 0
    fi

    local lockfile; lockfile="${SPIRA_QUEUE_DIR:?}/$name/lock"
    mkdir -p "${SPIRA_QUEUE_DIR:?}/$name" 2>/dev/null || true
    { exec 9>"$lockfile"; } 2>/dev/null \
        || { printf 'batch %s: cannot open lock file\n' "$name" >&2; return 1; }
    if ! flock -w "${SPIRA_QUEUE_LOCK_WAIT:-90}" 9; then
        local _skips_file _skips
        _skips_file="${SPIRA_QUEUE_DIR:?}/$name/lock-skips"
        _skips=$(( $(cat "$_skips_file" 2>/dev/null || printf '0') + 1 ))
        printf '%d\n' "$_skips" > "$_skips_file" 2>/dev/null || true
        if [ "$_skips" -eq "${SPIRA_QUEUE_LOCK_STARVE_MAX:-5}" ]; then
            printf 'batch %s: queue lock starvation — skipped %d consecutive ticks waiting for lock\n' \
                "$name" "$_skips"
        else
            printf 'batch %s: another queue operation holds the lock\n' "$name"
        fi
        return 0
    fi
    rm -f "${SPIRA_QUEUE_DIR:?}/$name/lock-skips" 2>/dev/null || true

    base="$(spira_landref "$repo")" \
        || { printf 'batch %s: cannot resolve base ref\n' "$name" >&2; return 1; }
    base_sha="$(git -C "$repo" rev-parse "$base" 2>/dev/null)" \
        || { printf 'batch %s: cannot resolve %s\n' "$name" "$base" >&2; return 1; }
    remote="$(ref_remote "$base")"
    base_branch="$(ref_branch "$base")"

    # Orphan-run sweep: a spira/queue/* branch's Gate run left in-progress after its
    # PR closed (eviction/abandon/eject that predates queue_cancel_branch_runs, or a
    # PR closed by hand). The live batch's own branch is excluded because its PR is
    # still open.
    queue_sweep_orphan_runs "${SPIRA_FORGE:-$HERE/forge.sh}" "$repo" || true

    # Closed beads with RED/EJECTED landstates and live branches are unreachable: the batch
    # builder ignores closed beads and queue_certified_list selects on CERTIFIED. Log and
    # mail the operator so the loss is visible before a "queue drained" conclusion stands.
    local _crl_id _crl_ids _crl_list=""
    _crl_ids="$(_closed_red_live "$repo")"
    if [ -n "${_crl_ids:-}" ]; then
        while IFS= read -r _crl_id; do
            [ -n "$_crl_id" ] || continue
            printf 'batch %s: WARN closed-red-live %s — bead closed with landstate RED/EJECTED and branch spira/%s is alive\n' \
                "$name" "$_crl_id" "$_crl_id"
            local _crl_title
            _crl_title="$(timeout 5 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$_crl_id" --json 2>/dev/null \
                | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("title") or "")' 2>/dev/null || true)"
            _crl_list="${_crl_list}- ${_crl_id}${_crl_title:+ — ${_crl_title}}\n"
        done <<< "$_crl_ids"
        printf '## Note\nBead(s) for %s are closed with a RED/EJECTED landstate and a live branch:\n\n%bThese beads cannot re-enter the queue. Re-open and recertify each branch to resume.\n' \
            "$name" "$_crl_list" \
        | bash "$HERE/mail.sh" send operator \
            --from "Spira Queue <queue@spira>" \
            --subject "Merge queue: $name — closed bead(s) with live evicted branch" \
            2>/dev/null || true
    fi

    # An open batch's own back pressure (law-queue-back-pressure-is-an-open-pr) is the
    # batcher's to enforce now — an express or main-red trigger stacks onto it rather than
    # waiting or evicting it (batcher-cut's stack_round). All that is left here is the one
    # thing a stacked-onto PR cannot self-detect: it going DIRTY against the base out from
    # under it.
    if _batch_is_open "$name"; then
        local _ob_forge="${SPIRA_FORGE:-$HERE/forge.sh}"
        local _ob_file; _ob_file="$(_batch_open_file "$name")"
        local _ob_pr
        _ob_pr="$(grep '^pr=' "$_ob_file" 2>/dev/null | head -1)"; _ob_pr="${_ob_pr#pr=}"
        if [ -n "$_ob_pr" ]; then
            local _ob_mstat
            _ob_mstat="$("$_ob_forge" pr-mergeability "$repo" "$_ob_pr" 2>/dev/null)" \
                || _ob_mstat="UNKNOWN"
            if [ "${_ob_mstat:-UNKNOWN}" = "DIRTY" ]; then
                printf 'batch %s: PR %s is DIRTY (merge conflicts) — abandoning\n' \
                    "$name" "$_ob_pr"
                local _ob_dirty_members
                _ob_dirty_members="$(grep '^members=' "$_ob_file" 2>/dev/null | head -1)"
                _ob_dirty_members="${_ob_dirty_members#members=}"
                _abandon_open_batch "$name" "$_ob_forge" "$repo" "$_ob_file" "$_ob_pr" \
                    "$_ob_dirty_members" \
                    "Batch abandoned: PR had merge conflicts (DIRTY). Members returned to CERTIFIED for re-batching."
                printf '## Note\nBatch PR %s for %s was found unmergeable (DIRTY) and has been abandoned.\n\nMembers returned to CERTIFIED and will be re-batched on the next pass.\n' \
                    "$_ob_pr" "$name" \
                | bash "$HERE/mail.sh" send operator \
                    --from "Spira Queue <queue@spira>" \
                    --subject "Merge queue: $name — batch PR abandoned (conflicts)" \
                    2>/dev/null || true
                return 0
            fi
        fi
        printf 'batch %s: open batch exists — leaving the round to the batcher\n' "$name"
        return 0
    fi

    local certs
    certs="$(_certified_list "$repo")"

    # SECOND LINE OF DEFENCE: refuse admission whatever the landstate says if bd
    # confirms the bead is not closed. An empty answer (bd unreachable) is not a
    # confirmation — it is treated as "unknown", not "not closed" (gap G8 already
    # pins bd-unreachable as fail-open elsewhere in this pass).
    #
    # SUBMITTED IS ADMISSIBLE (sp-qsona). A work bead's own close is converted at aeon
    # teardown to open + SPIRA_SUBMITTED_LABEL, and only the landing pass closes it, once
    # its batch lands — so "open, carrying the submitted label" is exactly the state a
    # finished work bead waits in, and refusing it here would mean no work bead ever
    # batches. Every reopen that should bar admission (eject, rework, a red gate) goes
    # through bead_reopen, which withdraws CERTIFIED, and every eject strips the label.
    if [ -n "${certs:-}" ]; then
        local _sf_filt="" _sf_cl _sf_id _sf_st
        while IFS= read -r _sf_cl; do
            [ -n "$_sf_cl" ] || continue
            _sf_id="${_sf_cl%% *}"
            _sf_st="$(spira_bead_status "$_sf_id")"
            if [ -n "$_sf_st" ] && [ "$_sf_st" != closed ] \
               && ! bead_has_label "$(bdjson show "$_sf_id" 2>/dev/null)" \
                        "${SPIRA_SUBMITTED_LABEL:-spira-submitted}"; then
                printf 'batch %s: WARN not-closed %s — CERTIFIED landstate but bead status=%s; refusing admission\n' \
                    "$name" "$_sf_id" "$_sf_st"
                continue
            fi
            _sf_filt="${_sf_filt}${_sf_cl}"$'\n'
        done <<< "$certs"
        certs="${_sf_filt%$'\n'}"
    fi

    # Mark already-in-base certified tips LANDED so they do not consume batch slots
    # or inflate the wait trigger.
    if [ -n "${certs:-}" ]; then
        local _filt="" _cid _ctip _cepoch _ltip _gkf _stored_gk _current_gk
        while IFS= read -r _cl; do
            [ -n "$_cl" ] || continue
            read -r _cid _ctip _cepoch <<< "$_cl"
            _ltip="$(git -C "$repo" rev-parse "refs/heads/spira/$_cid" 2>/dev/null || true)"
            if [ -n "${_ltip:-}" ] && [ "$_ltip" != "$_ctip" ]; then
                land_mark "$_cid" CERTIFIED "$_ltip"
                printf 'batch %s: stale-certification %s — certified=%s live=%s — re-certified\n' \
                    "$name" "$_cid" "${_ctip:0:8}" "${_ltip:0:8}"
                if git -C "$repo" merge-base --is-ancestor "$_ltip" "$base_sha" 2>/dev/null; then
                    land_mark "$_cid" LANDED "$_ltip" already-in-base
                    bead_close_on_land "$_cid" "$_ltip" || true
                    printf 'batch %s: %s live tip already in %s — LANDED (already-in-base)\n' \
                        "$name" "$_cid" "$base"
                else
                    _gkf="$LANDSTATE/$_cid.gate-key"
                    if [ -f "$_gkf" ]; then
                        _stored_gk="$(cat "$_gkf" 2>/dev/null)"
                        _current_gk="$(compute_gate_key "$repo" "$name" "spira/$_cid" "$base" 2>/dev/null || true)"
                        if [ -n "$_stored_gk" ] && [ -n "$_current_gk" ] && [ "$_stored_gk" != "$_current_gk" ]; then
                            printf 'batch %s: stale-cert-key %s — gate changed; needs fresh gate\n' "$name" "$_cid"
                            rm -f "${SPIRA_RUN:-/nonexistent}/submitted/$_cid" 2>/dev/null || true
                            continue
                        fi
                    fi
                    _filt="${_filt}${_cid} ${_ltip} ${_cepoch}"$'\n'
                fi
            elif git -C "$repo" merge-base --is-ancestor "$_ctip" "$base_sha" 2>/dev/null; then
                land_mark "$_cid" LANDED "$_ctip" already-in-base
                bead_close_on_land "$_cid" "$_ctip" || true
                printf 'batch %s: %s tip already in %s — LANDED (already-in-base)\n' \
                    "$name" "$_cid" "$base"
            else
                _gkf="$LANDSTATE/$_cid.gate-key"
                if [ -f "$_gkf" ]; then
                    _stored_gk="$(cat "$_gkf" 2>/dev/null)"
                    _current_gk="$(compute_gate_key "$repo" "$name" "spira/$_cid" "$base" 2>/dev/null || true)"
                    if [ -n "$_stored_gk" ] && [ -n "$_current_gk" ] && [ "$_stored_gk" != "$_current_gk" ]; then
                        printf 'batch %s: stale-cert-key %s — gate changed; needs fresh gate\n' "$name" "$_cid"
                        rm -f "${SPIRA_RUN:-/nonexistent}/submitted/$_cid" 2>/dev/null || true
                        continue
                    fi
                fi
                _filt="${_filt}${_cl}"$'\n'
            fi
        done <<< "$certs"
        certs="${_filt%$'\n'}"
    fi

    if [ -z "${certs:-}" ]; then
        rm -f "$SPIRA_RUN/queue-stuck-$name" 2>/dev/null || true
        printf 'batch %s: no cut — 0 certified\n' "$name"
        printf 'QUEUE NOCUT %s repo=%s certified=0\n' "$(date +%s)" "$name" \
            >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
        return 0
    fi

    local count now oldest_epoch age
    count="$(printf '%s\n' "$certs" | grep -c .)"
    now="$(date +%s)"
    oldest_epoch="$(printf '%s\n' "$certs" | awk '{print $3}' | sort -n | head -1)"
    age=$(( now - oldest_epoch ))

    # Measure time since the queue last made progress (BATCHED or LANDED), not the
    # age of the oldest waiting branch. A deep but draining queue has old certs
    # yet recent movement; measuring the cert age alone fires on depth, not stall.
    local last_moved; last_moved="$(queue_last_moved "$LANDSTATE")"

    local _stuck_flag="$SPIRA_RUN/queue-stuck-$name" _stuck_exists=0 _stuck_action
    [ -f "$_stuck_flag" ] && _stuck_exists=1
    # A queue can only be stuck on work that has been waiting: if the oldest
    # certification is newer than the last BATCHED/LANDED move, the stall clock
    # starts there, not at the move — otherwise an idle stretch with no waiting
    # work pages the moment the first branch is certified (sp-w4tyd).
    local _stall_from="$last_moved"
    [ "$oldest_epoch" -gt "$_stall_from" ] && _stall_from="$oldest_epoch"
    _stuck_action="$(queue_stuck_action "$last_moved" "$now" "${SPIRA_QUEUE_STUCK_AGE:-7200}" "$_stuck_exists" "$_stall_from")"
    case "$_stuck_action" in
        no-history)
            printf 'batch %s: no BATCHED/LANDED record — stuck check skipped\n' "$name"
            ;;
        not-stuck)
            rm -f "$_stuck_flag" 2>/dev/null || true
            ;;
        alert)
            local stuck_age=$(( now - _stall_from ))
            printf '## Note\nThe merge queue for %s has not made progress in %ds (threshold %ds).\n\nQueue depth: %d branch(es). This may indicate a conflict loop or a stalled batch builder.\n' \
                "$name" "$stuck_age" "${SPIRA_QUEUE_STUCK_AGE:-7200}" "$count" \
            | bash "$HERE/mail.sh" send operator \
                --from "Spira Queue <queue@spira>" \
                --subject "Merge queue: $name queue stuck (${stuck_age}s)" \
                2>/dev/null && touch "$_stuck_flag" 2>/dev/null || true
            [ -f "$_stuck_flag" ] && \
                printf 'batch %s: mailed operator about stuck queue (stuck_age %ds)\n' "$name" "$stuck_age"
            ;;
        already-alerted) : ;;
    esac

    printf 'batch %s: %d certified — cut decision left to the batcher\n' "$name" "$count"
}

# Sourced (queue.sh wants format_batch, _base_conflict et al. without a real sweep run) vs
# executed: main runs only when this file is the entry point.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    main "$@"
fi
