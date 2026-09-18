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

LANDSTATE="$SPIRA_RUN/landstate"

land_state() {           # land_state <id> -> "<state> <tip> <at>" or empty
    local f="$LANDSTATE/$1"
    [ -r "$f" ] || return 1
    tr -d '\n' < "$f" 2>/dev/null
}
land_mark() {            # land_mark <id> <state> <tip> [reason]
    mkdir -p "$LANDSTATE" 2>/dev/null || return 0
    printf '%s %s %s %s' "$2" "${3:-none}" "$(date +%s)" "${4:-}" \
        > "$LANDSTATE/$1.$$" 2>/dev/null \
        && mv -f "$LANDSTATE/$1.$$" "$LANDSTATE/$1" 2>/dev/null
}

# SPIRA_QUEUE_REPRO_BATCH: seam for per-member reproduction in tests.
: "${SPIRA_QUEUE_REPRO_BATCH:=$HERE/testenv-batch.sh}"

_lg_repro_is_red() {   # _lg_repro_is_red <suites-csv> <repo-dir> <branch> -> 0 if red
    local suites="$1" repo="$2" br="$3" tmp rc
    tmp="$(mktemp -d)"
    SPIRA_BATCH_RESULTS="$tmp" bash "$SPIRA_QUEUE_REPRO_BATCH" \
        --mode serial --suites "$suites" "$br" >/dev/null 2>&1
    rc=$?
    rm -rf "$tmp"
    [ "$rc" -eq 1 ]
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

# _certified_orphans <repo-path> — print id for each CERTIFIED landstate with no branch ref
_certified_orphans() {
    local f id st
    [ -d "$LANDSTATE" ] || return 0
    for f in "$LANDSTATE/"*; do
        [ -f "$f" ] || continue
        id="$(basename "$f")"
        { read -r st _ < "$f"; } 2>/dev/null || continue
        [ "$st" = "CERTIFIED" ] || continue
        git -C "$1" show-ref --verify -q "refs/heads/spira/$id" 2>/dev/null && continue
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

main() {
    local name="${1:-}"
    [ -n "$name" ] || { printf 'batch.sh: repo name required\n' >&2; exit 1; }

    local repo base base_sha remote base_branch mode
    repo="$(repo_root "$name")" || { printf 'batch %s: no repo-map entry\n' "$name" >&2; return 1; }
    mode="$(repo_land "$name")"
    [ "$mode" = "queue" ] || return 0

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

    # CERTIFIED landstate records with no branch ref were deleted while queued.
    # batch.sh would skip them silently; log and mail the operator instead.
    local _orphan _orphans _orphan_list=""
    _orphans="$(_certified_orphans "$repo")"
    if [ -n "${_orphans:-}" ]; then
        while IFS= read -r _orphan; do
            [ -n "$_orphan" ] || continue
            printf 'batch %s: WARN certified-orphan %s — CERTIFIED landstate but branch spira/%s is gone\n' \
                "$name" "$_orphan" "$_orphan"
            _orphan_list="${_orphan_list}- ${_orphan}\n"
        done <<< "$_orphans"
        printf '## Note\nBranch(es) were CERTIFIED in the merge queue for %s but their refs are gone:\n\n%b\nThe next batch will not include them. Check the reap log for what deleted the ref.\n' \
            "$name" "$_orphan_list" \
        | bash "$HERE/mail.sh" send operator \
            --from "Spira Queue <queue@spira>" \
            --subject "Merge queue: $name — CERTIFIED branch(es) missing" \
            2>/dev/null || true
    fi

    if _batch_is_open "$name"; then
        printf 'batch %s: open batch exists — skipping\n' "$name"
        return 0
    fi

    local certs
    certs="$(_certified_list "$repo")"

    # Mark already-in-base certified tips LANDED so they do not consume batch slots
    # or inflate the wait trigger.
    if [ -n "${certs:-}" ]; then
        local _filt="" _cid _ctip _cepoch
        while IFS= read -r _cl; do
            [ -n "$_cl" ] || continue
            read -r _cid _ctip _cepoch <<< "$_cl"
            if git -C "$repo" merge-base --is-ancestor "$_ctip" "$base_sha" 2>/dev/null; then
                land_mark "$_cid" LANDED "$_ctip" already-in-base
                printf 'batch %s: %s tip already in %s — LANDED (already-in-base)\n' \
                    "$name" "$_cid" "$base"
            else
                _filt="${_filt}${_cl}"$'\n'
            fi
        done <<< "$certs"
        certs="${_filt%$'\n'}"
    fi

    # Mark CERTIFIED records whose branches are gone so they drop from the queue view.
    if [ -d "$LANDSTATE" ]; then
        local _lf _lid _lst _ltip _anyrn _rn _rp
        for _lf in "$LANDSTATE"/*; do
            [ -f "$_lf" ] || continue
            _lid="$(basename "$_lf")"
            # Skip entries that are not bead IDs (no slashes, no leading dot).
            case "$_lid" in .*|*/*) continue ;; esac
            { read -r _lst _ltip _ < "$_lf"; } 2>/dev/null || continue
            [ "$_lst" = "CERTIFIED" ] || continue
            _anyrn=0
            for _rn in $(spira_repos); do
                _rp="$(repo_root "$_rn" 2>/dev/null)" || continue
                git -C "$_rp" show-ref --verify --quiet "refs/heads/spira/$_lid" 2>/dev/null \
                    && { _anyrn=1; break; }
            done
            [ "$_anyrn" = 1 ] && continue
            land_mark "$_lid" LOST "${_ltip:-none}" branch-gone
            printf 'batch %s: %s has no branch — LOST (branch-gone)\n' "$name" "$_lid"
        done
    fi

    if [ -z "${certs:-}" ]; then
        rm -f "$SPIRA_RUN/queue-stuck-$name" 2>/dev/null || true
        return 0
    fi

    local count now oldest_epoch age triggered=
    count="$(printf '%s\n' "$certs" | grep -c .)"
    now="$(date +%s)"
    oldest_epoch="$(printf '%s\n' "$certs" | awk '{print $3}' | sort -n | head -1)"
    age=$(( now - oldest_epoch ))

    local _stuck_flag="$SPIRA_RUN/queue-stuck-$name"
    if [ "$age" -ge "${SPIRA_QUEUE_STUCK_AGE:-7200}" ] && [ ! -f "$_stuck_flag" ]; then
        printf '## Note\nThe oldest certified branch in %s has been waiting %ds (threshold %ds).\n\nQueue depth: %d branch(es). This may indicate a conflict loop or a stalled batch builder.\n' \
            "$name" "$age" "${SPIRA_QUEUE_STUCK_AGE:-7200}" "$count" \
        | bash "$HERE/mail.sh" send operator \
            --from "Spira Queue <queue@spira>" \
            --subject "Merge queue: $name queue stuck (${age}s)" \
            2>/dev/null && touch "$_stuck_flag" 2>/dev/null || true
        [ -f "$_stuck_flag" ] && \
            printf 'batch %s: mailed operator about stuck queue (age %ds)\n' "$name" "$age"
    fi

    [ "$count" -ge "${SPIRA_QUEUE_BATCH_MAX:-8}" ] && triggered=1
    [ "$age"   -ge "${SPIRA_QUEUE_BATCH_WAIT:-1800}" ] && triggered=1
    [ -n "$triggered" ] || return 0

    # Collect IDs for a bulk priority query.
    local all_ids=()
    while read -r _id _ _; do all_ids+=("$_id"); done <<< "$certs"
    local prio_json
    prio_json="$(bdjson show "${all_ids[@]}" 2>/dev/null)" || prio_json="[]"

    # Sort: suite-transition first, then priority asc, then epoch asc.
    # queue_sort_rows (lib.sh) is the canonical implementation shared with the cockpit.
    local sortfile; sortfile="$(mktemp)"
    # shellcheck disable=SC2064
    trap "rm -f '$sortfile'" RETURN
    PRIO_JSON="$prio_json" queue_sort_rows "$repo" "$base_sha" \
        < <(printf '%s\n' "$certs") > "$sortfile"

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
        read -r _ _ _ _bid _btip <<< "$_line"
        if git -C "$wt" merge --no-edit --no-ff -m "spira: land $_bid" "$_btip" \
               >/dev/null 2>&1; then
            members+=("$_bid:$_btip")
            member_ids+=("$_bid")
            taken=$(( taken + 1 ))
        else
            git -C "$wt" merge --abort 2>/dev/null || true
            if _base_conflict "$repo" "$base_sha" "$_btip"; then
                # Conflict with the land ref itself — reopen the bead.
                bead_reopen "$_bid" \
                    "Reopened by batch builder: branch spira/$_bid conflicts with $base in $name." \
                    >/dev/null 2>&1 || true
                land_mark "$_bid" RED "$_btip" conflicts-with-base
                printf 'batch %s: %s conflicts with %s — reopened\n' "$name" "$_bid" "$base"
            else
                # Clean merge with the land ref: conflict is only with batch accumulation — skip.
                printf 'batch %s: %s conflicts with batch — skipped\n' "$name" "$_bid"
            fi
        fi
    done < "$sortfile"

    [ "${#members[@]}" -gt 0 ] || return 0

    local stamp batch_br batch_head
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    batch_br="spira/queue/$stamp"
    batch_head="$(git -C "$wt" rev-parse HEAD 2>/dev/null)"

    # LOCAL GATE: run the gate on the combined batch before opening a PR.
    # Green → push + open PR. Red → attribute, eject, rebuild, gate again.
    local lg_out lg_rc lg_start lg_cost
    lg_start="$(date +%s)"
    git -C "$repo" branch -f "$batch_br" "$batch_head" 2>/dev/null || true
    lg_out="$(SPIRA_GATE_BEAD="batch-$stamp" bash "$HERE/gate.sh" "$batch_br" "$name" 2>&1)"
    lg_rc=$?
    lg_cost=$(( $(date +%s) - lg_start ))

    if [ "$lg_rc" -ne 0 ] && spira_gate_blames_branch "$lg_rc"; then
        local lg_attr_start lg_attr_cost lg_ejected lg_suites_csv
        lg_attr_start="$(date +%s)"
        lg_ejected=""
        lg_suites_csv="$(_lg_red_suites "$lg_out")"

        local lg_survivors=() lg_ejected_arr=() _lmm _lmid _lmtip
        for _lmm in "${members[@]}"; do
            _lmid="${_lmm%%:*}"; _lmtip="${_lmm##*:}"
            if _lg_repro_is_red "${lg_suites_csv:-}" "$repo" "spira/$_lmid"; then
                lg_ejected_arr+=("$_lmm")
            else
                lg_survivors+=("$_lmm")
            fi
        done

        for _lmm in "${lg_ejected_arr[@]:-}"; do
            [ -n "$_lmm" ] || continue
            _lmid="${_lmm%%:*}"; _lmtip="${_lmm##*:}"
            bead_reopen "$_lmid" \
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
                return 0
            fi

            # Rebuild batch from survivors.
            members=(); member_ids=()
            git -C "$wt" reset -q --hard "$base_sha" 2>/dev/null
            git -C "$wt" clean -qfd 2>/dev/null || true
            for _lmm in "${lg_survivors[@]}"; do
                _lmid="${_lmm%%:*}"; _lmtip="${_lmm##*:}"
                if git -C "$wt" merge --no-edit --no-ff -m "spira: land $_lmid" "$_lmtip" \
                       >/dev/null 2>&1; then
                    members+=("$_lmid:$_lmtip")
                    member_ids+=("$_lmid")
                fi
            done

            if [ "${#members[@]}" -eq 0 ]; then
                SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$batch_br" 2>/dev/null || true
                return 0
            fi

            # Delete old batch branch; new stamp for the rebuilt batch.
            SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$batch_br" 2>/dev/null || true
            batch_head="$(git -C "$wt" rev-parse HEAD 2>/dev/null)"
            stamp="$(date -u +%Y%m%dT%H%M%SZ)"
            batch_br="spira/queue/$stamp"

            # Gate the rebuilt batch.
            lg_start="$(date +%s)"
            git -C "$repo" branch -f "$batch_br" "$batch_head" 2>/dev/null || true
            lg_out="$(SPIRA_GATE_BEAD="batch-$stamp" bash "$HERE/gate.sh" "$batch_br" "$name" 2>&1)"
            lg_rc=$?
            lg_cost=$(( $(date +%s) - lg_start ))

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

    if ! git -C "$repo" push -q "$remote" \
           "${batch_head}:refs/heads/${batch_br}" 2>/dev/null; then
        printf 'batch %s: could not push %s\n' "$name" "$batch_br" >&2
        return 1
    fi

    # Open PR via forge seam.
    local pr_body pr_n
    pr_body="$(
        printf 'queue: %d branch(es)\n\n' "${#members[@]}"
        for mid in "${member_ids[@]}"; do printf -- '- %s\n' "$mid"; done
    )"
    pr_n="$(printf '%s' "$pr_body" \
            | "$forge" pr-create "$repo" "$batch_br" "$base_branch" \
                "queue: ${#members[@]} branches" 2>/dev/null)" || {
        printf 'batch %s: forge pr-create failed for %s\n' "$name" "$batch_br" >&2
        return 1
    }
    [ -n "${pr_n:-}" ] || {
        printf 'batch %s: forge returned no PR number for %s\n' "$name" "$batch_br" >&2
        return 1
    }

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

    printf 'batch %s: PR %s opened — %d branches (%s)\n' \
        "$name" "$pr_n" "${#members[@]}" "$batch_br"
}

main "$@"
