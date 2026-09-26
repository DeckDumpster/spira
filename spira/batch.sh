#!/usr/bin/env bash
# batch.sh — merge-queue batch builder.
# Usage: batch.sh <repo-name>
#
# Opens a batch when the repository has no open batch and either
# SPIRA_QUEUE_BATCH_MAX certified branches exist or the oldest certified
# branch has waited SPIRA_QUEUE_BATCH_WAIT seconds. Branches are ordered:
# suite-state transitions first, then bead priority, then certification time.
# A branch that conflicts with the accumulating batch is skipped (stays
# CERTIFIED); a branch that conflicts with the land ref itself is reopened.
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

# SPIRA_QUEUE_REPRO_BATCH: seam for per-member reproduction in tests.
: "${SPIRA_QUEUE_REPRO_BATCH:=$HERE/testenv-batch.sh}"

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

# _pf_over <name> <stage> — the wall was hit: say so, name what to evict, and let CI decide.
_pf_over() {
    local slow
    slow="$(bash "$HERE/tsd-query.sh" slow-in-branch "$batch_br" 5 2>/dev/null | python3 -c '
import json, sys
try:
    rows = json.load(sys.stdin)
except Exception:
    rows = []
print(",".join("%ss %s" % (r["wall_secs"], r["suite"]) for r in rows))
' 2>/dev/null)"
    printf 'batch %s: pre-flight hit its %ss wall during %s — opening the PR, CI decides. Slowest suites (evict candidates): %s\n' \
        "$1" "${SPIRA_PREFLIGHT_WALL_SECS:-240}" "$2" "${slow:-none recorded}"
    printf 'QUEUE PREFLIGHT_OVER %s repo=%s stage=%s wall=%s slowest=%s\n' \
        "$(date +%s)" "$1" "$2" "${SPIRA_PREFLIGHT_WALL_SECS:-240}" "${slow:-none}" \
        >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
}

_lg_red_suites() {   # _lg_red_suites <gate-output> -> comma-separated suite names
    printf '%s\n' "$1" | awk '{
        for (i = 1; i < NF; i++)
            if ($i ~ /\.sh$/ && ($(i+1) ~ /^(RED|TIMEOUT|FAILED)$/ ||
                ($(i+1) == "was" && $(i+2) == "killed")))
                if (!seen[$i]++) print $i
    }' | tr '\n' ',' | sed 's/,$//'
}

_batch_open_file() { printf '%s/%s/open' "${SPIRA_QUEUE_DIR:?}" "$1"; }

_batch_is_open() { [ -f "$(_batch_open_file "$1")" ]; }

# _abandon_open_batch <name> <forge> <repo> <ob_file> <pr_n> <members_val> <comment>
# Closes PR, returns innocent members to CERTIFIED, archives the open record.
# Single abandonment path — used by DIRTY and express eviction alike.
_abandon_open_batch() {
    local name="$1" forge="$2" repo="$3" ob_file="$4" pr_n="$5" members_val="$6" comment_text="${7:-Batch abandoned.}"
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

# _certified_orphans <repo-path> — print id for each CERTIFIED landstate with no branch ref
_certified_orphans() {
    local f id st
    [ -d "$LANDSTATE" ] || return 0
    for f in "$LANDSTATE/"*; do
        [ -f "$f" ] || continue
        id="$(basename "$f")"
        st=""; { read -r st _ < "$f"; } 2>/dev/null || [ -n "$st" ] || continue
        [ "$st" = "CERTIFIED" ] || continue
        git -C "$1" show-ref --verify -q "refs/heads/spira/$id" 2>/dev/null && continue
        printf '%s\n' "$id"
    done
}

# _closed_red_live <repo-path> — print id for each RED/EJECTED landstate whose branch
# exists and whose bead is closed. This is the counterpart to _certified_orphans: where
# that catches CERTIFIED with no branch (deleted while queued), this catches closed with
# a live branch and a failed landstate — the eviction-race shape where a bead ends up
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

# _landed_false <repo-path> <base-ref> — print "<id> <tip>" for each LANDED landstate
# whose tip is neither an ancestor of base nor provably its content (law-landed-is-content).
# THE SWEEP for sp-dgaig's defect: a LANDED record written on a REMOVED reap-log line alone,
# or on landed()'s old any-mention match, never actually reached base. Both writers are fixed
# elsewhere in this file and in lib.sh; this finds any record either left behind before the
# fix, or that a caller not yet audited still manages to write.
_landed_false() {
    local repo="$1" base="$2" f id st tip
    [ -d "$LANDSTATE" ] || return 0
    for f in "$LANDSTATE/"*; do
        [ -f "$f" ] || continue
        id="$(basename "$f")"
        case "$id" in .*|*/*) continue ;; esac
        st=""; tip=""
        { read -r st tip _ < "$f"; } 2>/dev/null || [ -n "$st" ] || continue
        [ "$st" = "LANDED" ] || continue
        [ -n "${tip:-}" ] && [ "$tip" != none ] || continue
        git -C "$repo" merge-base --is-ancestor "$tip" "$base" 2>/dev/null && continue
        content_landed "$repo" "$tip" "$base" 2>/dev/null && continue
        printf '%s %s\n' "$id" "$tip"
    done
}

# _landed_false_restore <id> — if a branch spira/<id> exists locally in any managed
# repository, or on that repository's own remote-tracking ref, print its tip sha. Restores
# the local ref from the remote-tracking one first, matching the by-hand recovery this
# replaces (`git branch spira/<id> <remote>/spira/<id>`; sp-dgaig).
_landed_false_restore() {
    local id="$1" rn rp rem
    for rn in $(spira_repos); do
        rp="$(repo_root "$rn" 2>/dev/null)" || continue
        if git -C "$rp" show-ref --verify -q "refs/heads/spira/$id" 2>/dev/null; then
            git -C "$rp" rev-parse "refs/heads/spira/$id" 2>/dev/null
            return 0
        fi
        rem="$(ref_remote "$(spira_landref "$rp" 2>/dev/null)" 2>/dev/null)" || continue
        if git -C "$rp" show-ref --verify -q "refs/remotes/$rem/spira/$id" 2>/dev/null &&
           git -C "$rp" branch "spira/$id" "refs/remotes/$rem/spira/$id" >/dev/null 2>&1; then
            git -C "$rp" rev-parse "refs/heads/spira/$id" 2>/dev/null
            return 0
        fi
    done
    return 1
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

# batch_cut_reason_cheap <count> <age-seconds> <max> <wait-seconds> <evict-for-express 0|1>
# -> prints the trigger reason (count|age|evict-express) and returns 0, or prints
# nothing and returns 1. Pure: the caller already resolved every input, so this needs
# no repo, no forge, no bd. Covers the two triggers that cost nothing to check and the
# internal express-eviction re-cut, in the order main() has always evaluated them.
batch_cut_reason_cheap() {
    local count="$1" age="$2" max="$3" wait="$4" evict="$5"
    if [ "$count" -ge "$max" ]; then printf 'count\n'; return 0; fi
    if [ "$age" -ge "$wait" ]; then printf 'age\n'; return 0; fi
    if [ "$evict" = 1 ]; then printf 'evict-express\n'; return 0; fi
    return 1
}

# batch_cut_idle <runs-active> -> 0 (cut) iff runs-active is the literal string "0".
# "?" and anything non-numeric must read as busy, never as idle — a forge that cannot
# answer must never be read as an empty queue (law-absence-needs-a-positive-control).
batch_cut_idle() {
    case "${1:-?}" in
        0) return 0 ;;
        *) return 1 ;;
    esac
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

# batch_cut_express <prio-json> <express-label> -> 0 iff any bead in prio-json carries
# the label. prio-json is the same bdjson-show array the sort step already fetched.
batch_cut_express() {
    PRIO_JSON="$1" EXPRESS_LABEL="$2" python3 -c '
import sys, json, os
d = json.loads(os.environ.get("PRIO_JSON", "[]") or "[]")
d = d if isinstance(d, list) else [d]
lbl = os.environ.get("EXPRESS_LABEL", "express")
sys.exit(0 if any(lbl in (b.get("labels") or []) for b in d) else 1)
' 2>/dev/null
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
    if ! flock -n 9; then
        printf 'batch %s: another queue operation holds the lock\n' "$name"
        return 0
    fi

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

    # CERTIFIED landstate records with no branch ref were deleted while queued.
    # batch.sh would skip them silently; log and mail the operator instead.
    local _orphan _orphans _orphan_list=""
    _orphans="$(_certified_orphans "$repo")"
    if [ -n "${_orphans:-}" ]; then
        while IFS= read -r _orphan; do
            [ -n "$_orphan" ] || continue
            printf 'batch %s: WARN certified-orphan %s — CERTIFIED landstate but branch spira/%s is gone\n' \
                "$name" "$_orphan" "$_orphan"
            local _orphan_title
            _orphan_title="$(timeout 5 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$_orphan" --json 2>/dev/null \
                | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("title") or "")' 2>/dev/null || true)"
            _orphan_list="${_orphan_list}- ${_orphan}${_orphan_title:+ — ${_orphan_title}}\n"
        done <<< "$_orphans"
        printf '## Note\nBranch(es) were CERTIFIED in the merge queue for %s but their refs are gone:\n\n%b\nThe next batch will not include them. Check the reap log for what deleted the ref.\n' \
            "$name" "$_orphan_list" \
        | bash "$HERE/mail.sh" send operator \
            --from "Spira Queue <queue@spira>" \
            --subject "Merge queue: $name — CERTIFIED branch(es) missing" \
            2>/dev/null || true
    fi

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

    # LANDED landstate whose tip never reached base — the shape sp-dgaig fixed two writers
    # of. Report every one found, and recover what can be: if a branch for the id still
    # exists anywhere this harness can reach (locally, or on that repository's own remote),
    # restore it and re-certify so the queue picks the real work back up (law-a-sweep-is-not-
    # a-bead: this runs every batch pass rather than waiting to be noticed by hand).
    local _lf_id _lf_tip _lf_entry _lf_entries _lf_list=""
    _lf_entries="$(_landed_false "$repo" "$base_sha")"
    if [ -n "${_lf_entries:-}" ]; then
        while IFS= read -r _lf_entry; do
            [ -n "$_lf_entry" ] || continue
            _lf_id="${_lf_entry%% *}"
            local _lf_restored
            if _lf_restored="$(_landed_false_restore "$_lf_id")" && [ -n "$_lf_restored" ]; then
                land_mark "$_lf_id" CERTIFIED "$_lf_restored" recertified-false-landed
                printf 'batch %s: WARN false-landed %s — tip never reached %s; branch found and RE-CERTIFIED at %s\n' \
                    "$name" "$_lf_id" "$base" "$_lf_restored"
            else
                printf 'batch %s: WARN false-landed %s — tip never reached %s; no branch found anywhere, needs manual recovery\n' \
                    "$name" "$_lf_id" "$base"
            fi
            local _lf_title
            _lf_title="$(timeout 5 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$_lf_id" --json 2>/dev/null \
                | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("title") or "")' 2>/dev/null || true)"
            _lf_list="${_lf_list}- ${_lf_id}${_lf_title:+ — ${_lf_title}}\n"
        done <<< "$_lf_entries"
        printf '## Note\nBead(s) for %s were recorded LANDED but their tip never reached %s:\n\n%bSee the batch log for which were re-certified and which need recovery by hand.\n' \
            "$name" "$base" "$_lf_list" \
        | bash "$HERE/mail.sh" send operator \
            --from "Spira Queue <queue@spira>" \
            --subject "Merge queue: $name — LANDED record(s) never reached base" \
            2>/dev/null || true
    fi

    # Mark CERTIFIED records whose branches are gone so they drop from the queue view.
    # This runs before the open-batch guard so stale records are converged even while a
    # batch PR is pending — without this, a long CI run leaves them accumulating
    # indefinitely (the guard returned early before this block ever ran).
    if [ -d "$LANDSTATE" ]; then
        local _lf _lid _lst _ltip _anyrn _rn _rp
        for _lf in "$LANDSTATE"/*; do
            [ -f "$_lf" ] || continue
            _lid="$(basename "$_lf")"
            # Skip entries that are not bead IDs (no slashes, no leading dot).
            case "$_lid" in .*|*/*) continue ;; esac
            _lst=""; _ltip=""; { read -r _lst _ltip _ < "$_lf"; } 2>/dev/null || [ -n "$_lst" ] || continue
            [ "$_lst" = "CERTIFIED" ] || continue
            _anyrn=0
            for _rn in $(spira_repos); do
                _rp="$(repo_root "$_rn" 2>/dev/null)" || continue
                git -C "$_rp" show-ref --verify --quiet "refs/heads/spira/$_lid" 2>/dev/null \
                    && { _anyrn=1; break; }
            done
            if [ "$_anyrn" = 1 ]; then
                # Branch still exists. Reconcile to LANDED when the certified tip is already
                # in the base AND the branch hasn't moved past it. If the branch has advanced
                # since certification, defer to the _certified_list stale-cert path (inside
                # the open-batch guard), which re-certifies with the live tip first.
                _lcur="$(git -C "$repo" rev-parse "refs/heads/spira/$_lid" 2>/dev/null || true)"
                if [ "${_lcur:-none}" = "${_ltip:-none}" ] && \
                   git -C "$repo" merge-base --is-ancestor "${_ltip:-none}" "$base_sha" \
                       2>/dev/null; then
                    land_mark "$_lid" LANDED "${_ltip:-none}" already-in-base
                    printf 'batch %s: %s tip already in %s (live branch) — LANDED\n' \
                        "$name" "$_lid" "$base"
                fi
                continue
            fi
            # Tip already in base: the branch landed (via external merge before verdict.sh
            # ran). Mark LANDED rather than LOST so the queue view and queue-wait logic
            # both see it as done — LOST drops it from the view but does not unblock
            # queue-waiters that tested for CERTIFIED reaching LANDED.
            if git -C "$repo" merge-base --is-ancestor "${_ltip:-none}" "$base_sha" \
                   2>/dev/null; then
                land_mark "$_lid" LANDED "${_ltip:-none}" already-in-base-orphan
                printf 'batch %s: %s tip already in %s (orphan) — LANDED\n' \
                    "$name" "$_lid" "$base"
            else
                # A REMOVED reap-log entry says something with this id was deleted, not that
                # its content is the content on base — the entry carries no tip and no date,
                # so it was true of any tip ever reaped under this id. Ask the question
                # content_landed asks instead, against the certified tip itself: it accepts
                # any commit-ish, so a tip whose branch is gone is judged the same way a live
                # one would be. A tip that was pruned (object no longer exists) fails the git
                # calls inside content_landed and this falls to LOST — refuse rather than
                # guess (sp-dgaig: a REMOVED-alone reading marked five reaped branches LANDED
                # though none of their content had reached base).
                if content_landed "$repo" "${_ltip:-none}" "$base_sha" 2>/dev/null; then
                    land_mark "$_lid" LANDED "${_ltip:-none}" content-landed-orphan
                    printf 'batch %s: %s has no branch — content on %s (orphan) — LANDED\n' \
                        "$name" "$_lid" "$base"
                else
                    land_mark "$_lid" LOST "${_ltip:-none}" branch-gone
                    printf 'batch %s: %s has no branch — LOST (branch-gone)\n' "$name" "$_lid"
                fi
            fi
        done
    fi

    local _takeover_for_express=0 _ob_express_ids=()
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

        # An open batch is never evicted for express (Ryan, 2026-09-24): a batch that
        # might still land green is not spent to save an express bead a wait — it takes
        # the next slot instead, once this one closes (below, certs sort express-first).
        # The one exception is a batch CI has already resolved as not-green: waiting for
        # its local attribution to finish before cutting the express batch defeats the
        # fast lane, so a certified express branch takes the freed slot immediately and
        # the failed batch's own attribution — eject the culprit, re-certify survivors —
        # keeps running separately (verdict.sh, against the file this stashes it in).
        local _ob_ev_certs _ob_ev_ids=() _ob_ev_id _ob_ev_prio _ob_ev_errfile _ob_ev_lookup_failed=0
        _ob_ev_certs="$(_certified_list "$repo")"
        if [ -n "${_ob_ev_certs:-}" ]; then
            while read -r _ob_ev_id _ _; do _ob_ev_ids+=("$_ob_ev_id"); done <<< "$_ob_ev_certs"
            if [ "${#_ob_ev_ids[@]}" -gt 0 ]; then
                # bdq show, not bdjson: bdjson swallows bdq's own stderr internally, and a
                # `|| fallback="[]"` here cannot be told apart from bdq genuinely finding no
                # express beads — both read as "nothing express", the case
                # law-a-control-that-cannot-check-must-refuse names. A failed lookup must
                # say so and refuse to fall through to the ordinary skip line.
                _ob_ev_errfile="$(mktemp)"
                if _ob_ev_prio="$(bdq show "${_ob_ev_ids[@]}" --json 2>"$_ob_ev_errfile" | json_only)"; then
                    :
                else
                    _ob_ev_lookup_failed=1
                    printf 'batch %s: express lookup FAILED: %s\n' \
                        "$name" "$(tr '\n' ' ' < "$_ob_ev_errfile")"
                fi
                rm -f "$_ob_ev_errfile"
                if [ "$_ob_ev_lookup_failed" -eq 0 ]; then
                    local _elab="${SPIRA_EXPRESS_LABEL:-express}"
                    while IFS= read -r _ob_ev_id; do
                        [ -n "$_ob_ev_id" ] && _ob_express_ids+=("$_ob_ev_id")
                    done < <(PRIO_JSON="$_ob_ev_prio" EXPRESS_LABEL="$_elab" python3 -c '
import sys, json, os
d = json.loads(os.environ.get("PRIO_JSON", "[]") or "[]")
d = d if isinstance(d, list) else [d]
lbl = os.environ.get("EXPRESS_LABEL", "express")
for b in d:
    if lbl in (b.get("labels") or []):
        print(b.get("id", ""))
' 2>/dev/null)
                fi
            fi
        fi

        if [ "$_ob_ev_lookup_failed" -eq 1 ]; then
            return 0
        fi

        if [ "${#_ob_express_ids[@]}" -gt 0 ] && [ -n "$_ob_pr" ]; then
            local _ob_status_out _ob_status
            _ob_status_out="$("$_ob_forge" check-status "$repo" "$_ob_pr" 2>/dev/null)" \
                || _ob_status_out="pending"
            _ob_status="$(printf '%s\n' "$_ob_status_out" | head -1)"
            _ob_status="${_ob_status:-pending}"
            case "$_ob_status" in
            green|pending) ;;   # may still land clean — express waits for the slot
            *)
                # Not green (red, harness_fault, provision_fault, or an unreadable
                # check-status): stash the open batch's record for verdict.sh to
                # attribute on its own, and free the slot for the express batch.
                local _ob_atdir _ob_ev_csv
                _ob_atdir="$(dirname "$_ob_file")"
                _ob_ev_csv="$(IFS=,; echo "${_ob_express_ids[*]}")"
                if mv -f "$_ob_file" "$_ob_atdir/attributing-$_ob_pr" 2>/dev/null; then
                    printf 'batch %s: open batch PR %s is %s — certified express branch takes over (%s); its attribution continues separately\n' \
                        "$name" "$_ob_pr" "${_ob_status:-unknown}" "$_ob_ev_csv"
                    printf '## Note\nOpen batch PR %s for %s resolved %s.\n\nExpress bead(s) %s took the next batch slot rather than waiting for local attribution to finish. The failed batch keeps attributing on its own; its survivors will be re-batched once the express batch lands.\n' \
                        "$_ob_pr" "$name" "${_ob_status:-unknown}" "$_ob_ev_csv" \
                    | bash "$HERE/mail.sh" send operator \
                        --from "Spira Queue <queue@spira>" \
                        --subject "Merge queue: $name — express takes over from batch $_ob_pr ($_ob_status)" \
                        2>/dev/null || true
                    printf 'QUEUE TAKEOVER %s repo=%s pr=%s reason=express express=%s status=%s\n' \
                        "$(date +%s)" "$name" "$_ob_pr" "$_ob_ev_csv" "${_ob_status:-unknown}" \
                        >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
                    _takeover_for_express=1
                fi
                ;;
            esac
        fi

        if [ "$_takeover_for_express" -eq 0 ]; then
            printf 'batch %s: open batch exists — skipping\n' "$name"
            return 0
        fi
        # Fall through: cut the express batch now, into the freed slot.
    fi

    local certs
    certs="$(_certified_list "$repo")"

    # SECOND LINE OF DEFENCE: refuse admission whatever the landstate says if bd
    # confirms the bead is not closed. An empty answer (bd unreachable) is not a
    # confirmation — it is treated as "unknown", not "not closed" (gap G8 already
    # pins bd-unreachable as fail-open elsewhere in this pass).
    if [ -n "${certs:-}" ]; then
        local _sf_filt="" _sf_cl _sf_id _sf_st
        while IFS= read -r _sf_cl; do
            [ -n "$_sf_cl" ] || continue
            _sf_id="${_sf_cl%% *}"
            _sf_st="$(spira_bead_status "$_sf_id")"
            if [ -n "$_sf_st" ] && [ "$_sf_st" != closed ]; then
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

    # Express takeover: build the batch from only the branches that triggered it — the
    # freed slot belongs to express, not to whatever else is CERTIFIED alongside it.
    if [ "${_takeover_for_express:-0}" = 1 ] && [ "${#_ob_express_ids[@]}" -gt 0 ]; then
        local _ecerts="" _ecl _ecid _is_express _eid
        while IFS= read -r _ecl; do
            [ -n "$_ecl" ] || continue
            read -r _ecid _ <<< "$_ecl"
            _is_express=0
            for _eid in "${_ob_express_ids[@]}"; do
                [ "$_ecid" = "$_eid" ] && { _is_express=1; break; }
            done
            [ "$_is_express" -eq 1 ] && _ecerts="${_ecerts}${_ecl}"$'\n'
        done <<< "$certs"
        certs="${_ecerts%$'\n'}"
    fi

    if [ -z "${certs:-}" ]; then
        rm -f "$SPIRA_RUN/queue-stuck-$name" 2>/dev/null || true
        printf 'batch %s: no cut — 0 certified\n' "$name"
        printf 'QUEUE NOCUT %s repo=%s certified=0\n' "$(date +%s)" "$name" \
            >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
        return 0
    fi

    local count now oldest_epoch age triggered=
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

    local _cheap_reason
    _cheap_reason="$(batch_cut_reason_cheap "$count" "$age" "${SPIRA_QUEUE_BATCH_MAX:-8}" \
        "${SPIRA_QUEUE_BATCH_WAIT:-1800}" "${_takeover_for_express:-0}")" && triggered=1

    # BISECT: a prior red with no attributable suite already narrowed the
    # culprit by persisted binary search (queue_bisect_split, run from
    # verdict.sh). Force the cut regardless of the other triggers, and force
    # its membership below to be exactly the recorded half — a fresh
    # priority-sorted cut would just re-select the same culprit forever
    # (sp-y931m: PRs 302-304 cycled the same P0 build-breaker).
    local _bisect_forced=""
    if [ -n "$(queue_bisect_current "$name" "$repo" "$base_sha")" ]; then
        _bisect_forced="$(queue_bisect_current_certified "$name" "$certs")"
        if [ -z "$_bisect_forced" ]; then
            queue_bisect_advance "$name"
            printf 'batch %s: bisect group already resolved elsewhere — advancing\n' "$name"
        fi
    fi
    [ -n "$_bisect_forced" ] && triggered=1

    # THIRD TRIGGER: CI IS IDLE, SO WAITING BUYS NOTHING. The wait exists to let certified
    # branches accumulate into one CI run instead of spending a run each. That trade is only
    # worth making while a run is in flight — with nothing in CI, a branch that waits its
    # full SPIRA_QUEUE_BATCH_WAIT is a branch held back from an idle machine for no gain.
    # The operator, verbatim (2026-09-23): "if there's NOTHING in CI, then we may as well
    # just send an immediate batch."
    #
    # ASKED LAST, AND ONLY WHEN IT CAN CHANGE THE ANSWER. This is a network round trip and
    # this function runs on every landing pass, so it is reached only when neither cheap
    # trigger fired and there is certified work waiting. When the other two already said yes,
    # the answer cannot matter.
    #
    # ? IS NOT ZERO. forge.sh runs-active prints ? when it cannot tell, and ? must never cut
    # a batch: reading a failed API call as "CI is idle" would fire this trigger on every
    # pass exactly when the forge is unreachable (law-absence-needs-a-positive-control).
    local _active=""
    if [ -z "$triggered" ] && [ "${SPIRA_QUEUE_BATCH_IDLE_CUT:-1}" = 1 ] && [ "$count" -gt 0 ]; then
        _active="$("${SPIRA_FORGE:-$HERE/forge.sh}" runs-active "$repo" 2>/dev/null)" || _active="?"
        if batch_cut_idle "$_active"; then
            triggered=1
            printf 'batch %s: CI idle (0 runs queued or in progress) — cutting %d certified branch(es) without waiting\n' \
                "$name" "$count"
        fi
    fi

    # Collect IDs for a bulk priority/labels query (also used for the express check).
    local all_ids=()
    while read -r _id _ _; do all_ids+=("$_id"); done <<< "$certs"
    local prio_json
    prio_json="$(bdjson show "${all_ids[@]}" 2>/dev/null)" || prio_json="[]"

    # EXPRESS: a certified express branch triggers a batch immediately.
    # A batch of one spends a full CI run on a single bead; knowingly accepted.
    if [ -z "$triggered" ]; then
        if batch_cut_express "$prio_json" "${SPIRA_EXPRESS_LABEL:-express}"; then
            printf 'batch %s: express certified branch — triggering immediate batch\n' "$name"
            triggered=1
        fi
    fi

    if [ -z "$triggered" ]; then
        local _age_m _wait_m _ci_status
        _age_m=$(( age / 60 ))
        _wait_m=$(( ${SPIRA_QUEUE_BATCH_WAIT:-1800} / 60 ))
        _ci_status=""
        case "${_active:-}" in
            '')              : ;;
            '?'|*[!0-9]*)  _ci_status="; CI unknown" ;;
            *)               _ci_status="; CI busy (${_active} active)" ;;
        esac
        printf 'batch %s: no cut — %d certified, need %d; oldest %dm, cuts at %dm%s\n' \
            "$name" "$count" "${SPIRA_QUEUE_BATCH_MAX:-8}" "$_age_m" "$_wait_m" "$_ci_status"
        printf 'QUEUE NOCUT %s repo=%s certified=%d need=%d age_seconds=%d wait_seconds=%d\n' \
            "$(date +%s)" "$name" "$count" "${SPIRA_QUEUE_BATCH_MAX:-8}" "$age" \
            "${SPIRA_QUEUE_BATCH_WAIT:-1800}" \
            >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
        return 0
    fi

    # Sort: express first, then suite-transition, then priority asc, then epoch asc.
    # queue_sort_rows (lib.sh) is the canonical implementation shared with the cockpit.
    # A forced bisect group bypasses this sort entirely — see the BISECT trigger above.
    local sortfile; sortfile="$(mktemp)"
    # shellcheck disable=SC2064
    trap "rm -f '$sortfile'" RETURN
    if [ -n "$_bisect_forced" ]; then
        printf '%s\n' "$_bisect_forced" | awk '{printf "0 0 000000000 0000000000 %s %s\n", $1, $2}' \
            > "$sortfile"
        printf 'batch %s: bisect in progress — forcing cut to recorded half (%d member(s))\n' \
            "$name" "$(printf '%s\n' "$_bisect_forced" | grep -c .)"
    else
        PRIO_JSON="$prio_json" queue_sort_rows "$repo" "$base_sha" \
            < <(printf '%s\n' "$certs") > "$sortfile"
    fi

    # Build batch in a worktree starting at the land ref.
    local wt
    wt="$SPIRA_RUN/worktree/.batch-$(basename "$repo")"
    # Prune stale registrations (e.g. a directory removed without worktree remove).
    git -C "$repo" worktree prune 2>/dev/null || true
    if [ -e "$wt/.git" ]; then
        git -C "$wt" reset -q --hard "$base_sha" 2>/dev/null
        git -C "$wt" clean -qfd 2>/dev/null || true
    else
        mkdir -p "$(dirname "$wt")"
        git -C "$repo" worktree add -q --detach "$wt" "$base_sha" 2>/dev/null \
            || { printf 'batch %s: cannot create batch worktree\n' "$name" >&2; return 1; }
    fi

    local max="${SPIRA_QUEUE_BATCH_MAX:-8}" taken=0
    local members=() member_ids=()
    local _bid _btip

    while IFS= read -r _line && [ "$taken" -lt "$max" ]; do
        read -r _ _ _ _ _bid _btip <<< "$_line"
        # Warn when the branch head has moved past the certified tip (sp-hm2vw). The
        # batch uses the certified tip; commits pushed after certification are not included
        # until the branch is re-certified.
        local _bcur
        _bcur="$(git -C "$repo" rev-parse "refs/heads/spira/$_bid" 2>/dev/null || true)"
        if [ -n "${_bcur:-}" ] && [ "$_bcur" != "$_btip" ]; then
            printf 'batch %s: WARN %s tip has moved since certification — certified=%s head=%s — using certified tip\n' \
                "$name" "$_bid" "${_btip:0:8}" "${_bcur:0:8}"
        fi
        unset _bcur
        if git -C "$wt" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" merge --no-edit --no-ff -m "spira: land $_bid" "$_btip" \
               >/dev/null 2>&1; then
            members+=("$_bid:$_btip")
            member_ids+=("$_bid")
            taken=$(( taken + 1 ))
        else
            git -C "$wt" merge --abort 2>/dev/null || true
            if _base_conflict "$repo" "$base_sha" "$_btip"; then
                local _cited_result="" _cited_sha="" _cited_rule="" _unlanded_ahead=""
                _cited_result="$(bead_cited_commit_on_base "$_bid" "$repo" "$base_sha" 2>/dev/null)" || true
                read -r _cited_sha _cited_rule <<< "$_cited_result"
                if [ -n "$_cited_sha" ]; then
                    _unlanded_ahead="$(git -C "$repo" rev-list "$base_sha..$_btip" 2>/dev/null)" \
                        || _unlanded_ahead=""
                    if [ -z "$_unlanded_ahead" ] \
                       || content_landed "$repo" "$_btip" "$base_sha" 2>/dev/null; then
                        land_mark "$_bid" LANDED "$_cited_sha" "${_cited_rule}-complete"
                        printf 'batch %s: %s notes cite %s (%s) already on %s — marked landed\n' \
                            "$name" "$_bid" "$_cited_sha" "${_cited_rule}-complete" "$base"
                    else
                        printf 'batch %s: %s notes cite %s (%s) on %s but branch has unlanded commits — reopening\n' \
                            "$name" "$_bid" "$_cited_sha" "$_cited_rule" "$base"
                        _cited_sha=""
                    fi
                fi
                if [ -z "$_cited_sha" ]; then
                    # Attempt rebase onto base before reopening.
                    local _rbwt _rbtip _rbrc _rbfp _rbconf
                    _rbwt="$SPIRA_RUN/worktree/.batch-rb-$$"
                    _rbtip="" _rbrc=1 _rbconf=""
                    if git -C "$repo" worktree add -q --detach "$_rbwt" "$_btip" 2>/dev/null; then
                        _rbfp="$(git -C "$_rbwt" merge-base HEAD "$base_sha" 2>/dev/null)" || _rbfp=""
                        if [ -n "$_rbfp" ]; then
                            git -C "$_rbwt" rebase --onto "$base_sha" "$_rbfp" \
                                >/dev/null 2>&1; _rbrc=$?
                            if [ "$_rbrc" -eq 0 ]; then
                                _rbtip="$(git -C "$_rbwt" rev-parse HEAD 2>/dev/null)"
                            else
                                _rbconf="$(git -C "$_rbwt" diff --name-only --diff-filter=U 2>/dev/null | tr '\n' ' ')"
                                _rbconf="${_rbconf% }"
                                git -C "$_rbwt" rebase --abort 2>/dev/null || true
                            fi
                        fi
                        git -C "$repo" worktree remove -f "$_rbwt" 2>/dev/null || true
                    fi
                    if [ "$_rbrc" -eq 0 ] && [ -n "$_rbtip" ]; then
                        # update-ref, not branch -f: the aeon's own worktree for this
                        # bead may still have spira/$_bid checked out, and branch -f
                        # refuses to move a ref checked out anywhere.
                        git -C "$repo" update-ref "refs/heads/spira/$_bid" "$_rbtip" 2>/dev/null || true
                        land_mark "$_bid" RED "$_btip" conflicts-with-base
                        printf 'batch %s: %s rebased onto %s — handed to the landing pass to re-certify outside the queue lock\n' \
                            "$name" "$_bid" "$base"
                    else
                        bump_requeue "$_bid" merge-conflict >/dev/null 2>&1 || true
                        bead_reopen "$_bid" rebase-conflict \
                            "$(conflict_reopen_note "$repo" "spira/$_bid" "$base" "$name" "$_rbconf" "batch builder")" \
                            >/dev/null 2>&1 || true
                        land_mark "$_bid" RED "$_btip" conflicts-with-base
                        printf 'batch %s: %s conflicts with %s — reopened\n' "$name" "$_bid" "$base"
                    fi
                    unset _rbwt _rbtip _rbrc _rbfp _rbconf
                fi
                unset _cited_result _cited_sha _cited_rule _unlanded_ahead
            else
                # Clean merge with the land ref: conflict is only with batch accumulation — skip.
                printf 'batch %s: %s conflicts with batch — skipped\n' "$name" "$_bid"
            fi
        fi
    done < "$sortfile"

    if [ "${#members[@]}" -eq 0 ]; then
        printf 'batch %s: no cut — %d certified but all conflicted or skipped\n' "$name" "$count"
        printf 'QUEUE NOCUT %s repo=%s certified=%d reason=all-conflicted\n' \
            "$(date +%s)" "$name" "$count" \
            >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
        return 0
    fi

    local stamp batch_br batch_head
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    batch_br="spira/queue/$stamp"
    format_batch "$wt" "$base_sha" "$name"
    batch_head="$(git -C "$wt" rev-parse HEAD 2>/dev/null)"

    # LOCAL GATE: run the gate on the combined batch before opening a PR.
    # Green → push + open PR. Red → attribute, eject, rebuild, gate again.
    local lg_out lg_rc lg_start lg_cost lg_ejected="" lg_suites_csv=""
    lg_start="$(date +%s)"
    git -C "$repo" branch -f "$batch_br" "$batch_head" 2>/dev/null || true
    # SPIRA_QUEUE_LOCAL_GATE=0 opens the PR without the local gate. The gate runs inside
    # the landing pass, so every batch paid 30-45 min of it -- with no verdict, no other
    # batch and no landing moving -- before CI ran the same corpus (sp-hrkwa).
    if [ "${SPIRA_QUEUE_LOCAL_GATE:-1}" = 0 ]; then
        lg_out=""; lg_rc=0; lg_cost=0
        printf 'batch %s: local gate skipped (SPIRA_QUEUE_LOCAL_GATE=0) — CI is the authority\n' "$name"
    else
        _PF_DEADLINE=$(( lg_start + ${SPIRA_PREFLIGHT_WALL_SECS:-240} ))
        lg_out="$(_pf_gate "$batch_br" "$name" "$stamp")"
        lg_rc=$?
        lg_cost=$(( $(date +%s) - lg_start ))
        if [ "$lg_rc" -eq 124 ]; then
            _pf_over "$name" gate
            lg_rc=0
        fi
    fi

    if [ "$lg_rc" -ne 0 ] && spira_gate_blames_branch "$lg_rc"; then
        local lg_attr_start lg_attr_cost lg_ejected lg_suites_csv
        lg_attr_start="$(date +%s)"
        lg_ejected=""
        lg_suites_csv="$(_lg_red_suites "$lg_out")"

        # ATTRIBUTION IN PARALLEL, INSIDE THE WALL. Each member reproduces the red suites
        # alone, all at once; a member whose reproduction did not finish before the wall is
        # a survivor — an unfinished run is not evidence against it, and CI still decides.
        local lg_survivors=() lg_ejected_arr=() _lmm _lmid _lmtip _pf_rdir _pf_left_s
        _pf_rdir="$(mktemp -d)"
        _pf_left_s="$(_pf_left)"
        for _lmm in "${members[@]}"; do
            _lmid="${_lmm%%:*}"
            (
                _t="$(mktemp -d)"
                SPIRA_BATCH_RESULTS="$_t" _pf_run "$_pf_left_s" bash "$SPIRA_QUEUE_REPRO_BATCH" \
                    --mode serial --suites "${lg_suites_csv:-}" "spira/$_lmid" >/dev/null 2>&1
                printf '%s' "$?" > "$_pf_rdir/$_lmid"
                rm -rf "$_t"
            ) &
        done
        wait
        for _lmm in "${members[@]}"; do
            _lmid="${_lmm%%:*}"; _lmtip="${_lmm##*:}"
            if [ "$(cat "$_pf_rdir/$_lmid" 2>/dev/null)" = 1 ]; then
                lg_ejected_arr+=("$_lmm")
            else
                lg_survivors+=("$_lmm")
            fi
        done
        [ "$(_pf_left)" -eq 0 ] && _pf_over "$name" attribution
        rm -rf "$_pf_rdir"

        for _lmm in "${lg_ejected_arr[@]:-}"; do
            [ -n "$_lmm" ] || continue
            _lmid="${_lmm%%:*}"; _lmtip="${_lmm##*:}"
            bead_reopen "$_lmid" batch-eject \
                "Ejected by local batch gate: spira/$_lmid reproduced failure in $name." \
                >/dev/null 2>&1 || true
            land_mark "$_lmid" EJECTED "$_lmtip"
            printf 'QUEUE CAUGHT %s branch=%s\n' "$(date +%s)" "$_lmid" \
                >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
            lg_ejected="$lg_ejected${lg_ejected:+ }$_lmid"
            printf 'batch %s: ejected %s — reproduced local gate failure\n' "$name" "$_lmid"
        done

        lg_attr_cost=$(( $(date +%s) - lg_attr_start ))
        printf 'QUEUE BATCH %s repo=%s members=%d gate_seconds=%d verdict=red attr_seconds=%d ejected=%s\n' \
            "$(date +%s)" "$name" "${#members[@]}" "$lg_cost" "$lg_attr_cost" \
            "${lg_ejected:--}" \
            >> "$SPIRA_RUN/landing.log" 2>/dev/null || true

        # ATTRIBUTION FOUND NOBODY — OPEN THE PR ANYWAY.
        #
        # Everything below rebuilds the batch from the survivors and gates it
        # again. When nothing was ejected, survivors IS members, so the "rebuilt"
        # batch re-merges the same commits onto the same base and produces a tree
        # byte-identical to the one that just failed. Gating it again is a
        # guaranteed-identical result, and at the end the members are marked
        # CERTIFIED and the branch deleted — so the next pass assembles exactly the
        # same batch and does it all over. On 2026-09-17 that ran two full gates
        # (~380s) per landing pass, indefinitely, while the queue drained nothing:
        # every batch logged `ejected=-` and no pull request was ever opened.
        #
        # AND A RED NOBODY REPRODUCES IS NOT EVIDENCE AGAINST ANY MEMBER. CI is the
        # authority for a batch (law-green-prs-merge-themselves); this gate buys
        # latency, not coverage (law-local-gates-buy-latency-not-coverage), and
        # leaving it able to veto means a failure it cannot attribute kills a batch
        # CI never sees. Let CI adjudicate.
        #
        # This is deliberately not a fix for sp-2f51e, which is WHY attribution
        # finds nobody — testenv-batch.sh runs against the production checkout
        # rather than the branch it is given, so every member reproduces
        # identically. This guard is correct on its own terms and stays correct
        # once that lands: with working attribution, a red the members genuinely do
        # not carry still belongs to CI.
        if [ "${#lg_ejected_arr[@]}" -eq 0 ]; then
            printf 'batch %s: local gate red (%s) but no member reproduced it — opening the PR; CI is the authority\n' \
                "$name" "${lg_suites_csv:-unattributed}"
        else
            if [ "${#lg_survivors[@]}" -eq 0 ]; then
                SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$batch_br" 2>/dev/null || true
                printf 'batch %s: no cut — all %d members ejected (%s)\n' \
                    "$name" "${#lg_ejected_arr[@]}" "${lg_suites_csv:-unknown}"
                printf 'QUEUE NOCUT %s repo=%s reason=all-ejected ejected=%s\n' \
                    "$(date +%s)" "$name" "${lg_ejected:--}" \
                    >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
                return 0
            fi

            # Rebuild batch from survivors.
            members=(); member_ids=()
            git -C "$wt" reset -q --hard "$base_sha" 2>/dev/null
            git -C "$wt" clean -qfd 2>/dev/null || true
            for _lmm in "${lg_survivors[@]}"; do
                _lmid="${_lmm%%:*}"; _lmtip="${_lmm##*:}"
                if git -C "$wt" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" merge --no-edit --no-ff -m "spira: land $_lmid" "$_lmtip" \
                       >/dev/null 2>&1; then
                    members+=("$_lmid:$_lmtip")
                    member_ids+=("$_lmid")
                fi
            done

            if [ "${#members[@]}" -eq 0 ]; then
                SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$batch_br" 2>/dev/null || true
                printf 'batch %s: no cut — rebuilt batch is empty\n' "$name"
                printf 'QUEUE NOCUT %s repo=%s reason=rebuilt-empty\n' \
                    "$(date +%s)" "$name" \
                    >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
                return 0
            fi

            # Delete old batch branch; new stamp for the rebuilt batch.
            SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$batch_br" 2>/dev/null || true
            format_batch "$wt" "$base_sha" "$name"
            batch_head="$(git -C "$wt" rev-parse HEAD 2>/dev/null)"
            stamp="$(date -u +%Y%m%dT%H%M%SZ)"
            batch_br="spira/queue/$stamp"

            # Gate the rebuilt batch.
            lg_start="$(date +%s)"
            git -C "$repo" branch -f "$batch_br" "$batch_head" 2>/dev/null || true
            lg_out="$(_pf_gate "$batch_br" "$name" "$stamp")"
            lg_rc=$?
            lg_cost=$(( $(date +%s) - lg_start ))
            if [ "$lg_rc" -eq 124 ]; then
                _pf_over "$name" re-gate
                lg_rc=0
            fi

            if [ "$lg_rc" -ne 0 ] && spira_gate_blames_branch "$lg_rc"; then
                for _lmm in "${lg_survivors[@]}"; do
                    _lmid="${_lmm%%:*}"; _lmtip="${_lmm##*:}"
                    land_mark "$_lmid" CERTIFIED "$_lmtip"
                done
                SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$batch_br" 2>/dev/null || true
                printf 'QUEUE BATCH %s repo=%s members=%d gate_seconds=%d verdict=red\n' \
                    "$(date +%s)" "$name" "${#members[@]}" "$lg_cost" \
                    >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
                printf 'batch %s: rebuilt batch also red — no PR opened\n' "$name"
                return 0
            fi
        fi
    fi

    # GREEN: log the batch meter line, push, open PR.
    printf 'QUEUE BATCH %s repo=%s members=%d gate_seconds=%d verdict=green\n' \
        "$(date +%s)" "$name" "${#members[@]}" "${lg_cost:-0}" \
        >> "$SPIRA_RUN/landing.log" 2>/dev/null || true

    local forge="${SPIRA_FORGE:-$HERE/forge.sh}"

    local _oqprs
    _oqprs="$("$forge" pr-list-queue "$repo" 2>/dev/null)" || _oqprs=""
    if [ -n "${_oqprs:-}" ]; then
        local _oqflag="$SPIRA_RUN/queue-unrecorded-pr-$name"
        if [ ! -f "$_oqflag" ]; then
            printf '## Note\nAn open spira/queue/* PR exists for %s that is not in the batch record.\n\nThis indicates concurrent batch builds raced. Inspect and close any orphan queue PR for %s.\n' \
                "$name" "$name" \
            | bash "$HERE/mail.sh" send operator \
                --from "Spira Queue <queue@spira>" \
                --subject "Merge queue: $name — unrecorded open queue PR" \
                2>/dev/null && touch "$_oqflag" 2>/dev/null || true
        fi
        printf 'batch %s: open queue PR not in batch record — skipping\n' "$name"
        return 0
    fi

    if ! spira_git_push "$repo" -q "$remote" \
           "${batch_head}:refs/heads/${batch_br}" 2>/dev/null; then
        printf 'batch %s: could not push %s\n' "$name" "$batch_br" >&2
        return 1
    fi

    # Open PR via forge seam.
    local member_titles_json
    member_titles_json="$(bdjson show "${member_ids[@]}" 2>/dev/null)" || member_titles_json="[]"

    local pr_body pr_n
    pr_body="$(
        printf 'Merge-queue batch: %d beads for %s, onto %s.\n\n' \
            "${#members[@]}" "$name" "$base_branch"
        for mid in "${member_ids[@]}"; do
            t="$(printf '%s\n' "$member_titles_json" | python3 -c "
import json, sys
data = json.load(sys.stdin)
items = data if isinstance(data, list) else [data]
t = next((str(i.get('title','')) for i in items if i.get('id') == '$mid'), '')
print((t[:120] if t else '(title unavailable)') or '(title unavailable)')
" 2>/dev/null)" || t="(title unavailable)"
            printf -- '- %s — %s\n' "$mid" "${t:-(title unavailable)}"
        done
    )"
    pr_n="$(printf '%s' "$pr_body" \
            | "$forge" pr-create "$repo" "$batch_br" "$base_branch" \
                "queue: ${#members[@]} beads for $name" 2>/dev/null)" || {
        printf 'batch %s: forge pr-create failed for %s\n' "$name" "$batch_br" >&2
        return 1
    }
    [ -n "${pr_n:-}" ] || {
        printf 'batch %s: forge returned no PR number for %s\n' "$name" "$batch_br" >&2
        return 1
    }

    # Post a comment for each ejected member so the PR body stays accurate.
    if [ -n "${lg_ejected:-}" ]; then
        local ej_titles_json ej_id ej_title ej_n_remain
        ej_n_remain="${#members[@]}"
        # shellcheck disable=SC2086
        ej_titles_json="$(bdjson show $lg_ejected 2>/dev/null)" || ej_titles_json="[]"
        for ej_id in $lg_ejected; do
            ej_title="$(printf '%s\n' "$ej_titles_json" | python3 -c "
import json, sys
data = json.load(sys.stdin)
items = data if isinstance(data, list) else [data]
t = next((str(i.get('title','')) for i in items if i.get('id') == '$ej_id'), '')
print((t[:120] if t else '(title unavailable)') or '(title unavailable)')
" 2>/dev/null)" || ej_title="(title unavailable)"
            "$forge" pr-comment "$repo" "$pr_n" \
                "Ejected: $ej_id — ${ej_title:-(title unavailable)} (${lg_suites_csv:-unknown}); $ej_n_remain remain" \
                2>/dev/null || true
        done
    fi

    # Record the open batch.
    local bdir; bdir="$(dirname "$(_batch_open_file "$name")")"
    mkdir -p "$bdir"
    {
        printf 'pr=%s\n'      "$pr_n"
        printf 'head=%s\n'    "$batch_head"
        printf 'base=%s\n'    "$base_sha"
        printf 'members=%s\n' "${members[*]}"
        printf 'opened=%s\n'  "$now"
        printf 'branch=%s\n'  "$batch_br"
    } > "$(_batch_open_file "$name")"

    # Mark each member BATCHED.
    local _mm _mid _mtip
    for _mm in "${members[@]}"; do
        _mid="${_mm%%:*}"; _mtip="${_mm##*:}"
        land_mark "$_mid" BATCHED "$_mtip"
    done
    rm -f "$SPIRA_RUN/queue-stuck-$name" 2>/dev/null || true

    # QUEUE BATCH's verdict=green line is written before push/PR-create can still fail,
    # so it cannot stand in for "a PR actually opened" — a watch on landing.log needs its
    # own line naming the PR number once one exists.
    local _ob_opened_msg
    _ob_opened_msg="$(printf 'batch %s: PR %s opened — %d branches (%s)' \
        "$name" "$pr_n" "${#members[@]}" "$batch_br")"
    printf '%s\n' "$_ob_opened_msg"
    printf '%s\n' "$_ob_opened_msg" >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
}

# Sourced (a T1 suite wants batch_cut_reason_cheap et al. without a real batch run) vs
# executed: main runs only when this file is the entry point.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    main "$@"
fi
