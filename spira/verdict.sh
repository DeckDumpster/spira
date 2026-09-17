#!/usr/bin/env bash
# verdict.sh — merge-queue verdict: read CI result, fast-forward or handle faults.
# Usage: verdict.sh <repo-name>
#
# Reads the open batch file for a queue-mode repository. Checks the PR's gate
# check status through the forge seam. Green with an unchanged base fast-forwards
# the land ref and marks every member LANDED. A moved base closes the PR and
# returns members to CERTIFIED. Pending does nothing unless the batch has waited
# longer than SPIRA_QUEUE_CI_MAXSEC. A harness fault re-runs the workflow up to
# SPIRA_QUEUE_INFRA_RETRIES times, then mails the operator. A red batch runs local
# reproduction per member: reproducers are ejected, together-only reds halve the
# batch, and unreproduced reds quarantine their suites and requeue.
#
# covers: spira/verdict.sh spira/forge.sh spira/conf.sh spira/batch.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

LANDSTATE="$SPIRA_RUN/landstate"

land_mark() {   # land_mark <id> <state> <tip> [reason]
    mkdir -p "$LANDSTATE" 2>/dev/null || return 0
    printf '%s %s %s %s' "$2" "${3:-none}" "$(date +%s)" "${4:-}" \
        > "$LANDSTATE/$1.$$" 2>/dev/null \
        && mv -f "$LANDSTATE/$1.$$" "$LANDSTATE/$1" 2>/dev/null
}

land_mark_at() {   # land_mark_at <id> <state> <tip> <epoch>
    mkdir -p "$LANDSTATE" 2>/dev/null || return 0
    printf '%s %s %s' "$2" "${3:-none}" "$4" \
        > "$LANDSTATE/$1.$$" 2>/dev/null \
        && mv -f "$LANDSTATE/$1.$$" "$LANDSTATE/$1" 2>/dev/null
}

_batch_open_file() { printf '%s/%s/open' "${SPIRA_QUEUE_DIR:?}" "$1"; }

_batch_field() {    # _batch_field <key> <file>
    grep "^$1=" "$2" 2>/dev/null | cut -d= -f2-
}

_batch_set_retries() {  # _batch_set_retries <file> <n>
    local f="$1" n="$2"
    { grep -v '^retries=' "$f"; printf 'retries=%s\n' "$n"; } > "$f.$$" \
        && mv -f "$f.$$" "$f"
}

# ---------------------------------------------------------------------------
# ATTRIBUTION — local per-member reproduction on a red batch.
# ---------------------------------------------------------------------------

# SPIRA_QUEUE_REPRO_BATCH allows tests to substitute testenv-batch.sh.
: "${SPIRA_QUEUE_REPRO_BATCH:=$HERE/testenv-batch.sh}"

_repro_is_red() {   # _repro_is_red <suites-csv> <repo-dir> <branch> -> 0 if red
    local suites="$1" repo="$2" br="$3" tmp rc
    tmp="$(mktemp -d)"
    SPIRA_BATCH_RESULTS="$tmp" bash "$SPIRA_QUEUE_REPRO_BATCH" \
        --mode serial --suites "$suites" "$br" >/dev/null 2>&1
    rc=$?
    rm -rf "$tmp"
    [ "$rc" -eq 1 ]
}

_any_suite_in_selection() {  # _any_suite_in_selection <suites-spacesep> <repo> <base-sha> <tip> -> 0 if any matches
    local suites="$1" repo="$2" base="$3" tip="$4" tmp sel s
    tmp="$(mktemp)"
    git -C "$repo" diff --name-only "$base" "$tip" 2>/dev/null > "$tmp" || true
    sel="$(bash "$HERE/select.sh" --files "$tmp" --repo "$repo" 2>/dev/null)"
    rm -f "$tmp"
    for s in $suites; do printf '%s\n' "$sel" | grep -qxF "$s" && return 0; done
    return 1
}

_quarantine_suites() {  # _quarantine_suites <suites-spacesep>
    local s; for s in $1; do
        SPIRA_FLAKE_QUARANTINE_AT=1 bash "$HERE/suites.sh" observe-flake "$s" 2>/dev/null || true
    done
}

_meter_write() {  # _meter_write <repo> <members> <caught> <escaped> <start-epoch>
    local name="$1" members="$2" caught="$3" escaped="$4" start="$5"
    local cost=$(( $(date +%s) - start ))
    printf 'QUEUE BATCH %s repo=%s members=%d caught=%d escaped=%d cost=%ds\n' \
        "$(date +%s)" "$name" "$members" "$caught" "$escaped" "$cost" \
        >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
}

_attr_eject() {  # _attr_eject <id> <tip> <suites-csv> <pr-n> <name>
    local id="$1" tip="$2" suites="$3" pr_n="$4" name="$5"
    bead_reopen "$id" \
        "Ejected by merge-queue attribution: spira/$id reproduced failure ($suites) from PR $pr_n in $name. Run those suites against this branch to reproduce." \
        >/dev/null 2>&1 || true
    land_mark "$id" EJECTED "$tip"
    printf 'QUEUE ESCAPED %s branch=%s\n' "$(date +%s)" "$id" \
        >> "$SPIRA_RUN/landing.log" 2>/dev/null || true
    printf '## Note\n%s was ejected from the merge queue after reproducing CI failures in PR %s (%s).\n\nFailing suites: %s\n\nRun those suites against spira/%s to reproduce.\n' \
        "$id" "$pr_n" "$name" "$suites" "$id" \
    | SPIRA_MAIL_LINT_CONSIDERED="queue-ejection" \
      bash "$HERE/mail.sh" send operator \
        --from "Spira Queue <queue@spira>" \
        --subject "Merge queue: $id ejected from $name" \
        2>/dev/null || true
    printf 'verdict %s: ejected %s (suites: %s)\n' "$name" "$id" "$suites"
}

_q_attribute() {
    local name="$1" repo="$2" pr_n="$3" batch_file="$4" branch_name="$5"
    local batch_head="$6" base_sha="$7" forge="$8" status_out="$9"
    local members_str="${10}"

    local attr_start; attr_start="$(date +%s)"

    # Parse red suite names from check-status output.
    local red_suites="" _line
    while IFS= read -r _line; do
        case "$_line" in "red-suite: "*) red_suites="$red_suites ${_line#red-suite: }" ;; esac
    done <<< "$status_out"
    red_suites="${red_suites# }"

    if [ -z "$red_suites" ]; then
        printf 'verdict %s: PR %s red — no suites identified; leaving batch open\n' "$name" "$pr_n"
        return 0
    fi

    local suites_csv; suites_csv="$(printf '%s\n' $red_suites | tr '\n' ',' | sed 's/,$//')"
    local members_arr=(); read -ra members_arr <<< "$members_str"
    local mc="${#members_arr[@]}"
    local caught=0 escaped=0
    local ejected=() survivors=()
    local _mm _mid _mtip

    if [ "$mc" -eq 1 ]; then
        _mm="${members_arr[0]}"; _mid="${_mm%%:*}"; _mtip="${_mm##*:}"
        local _unrep_dir="$SPIRA_QUEUE_DIR/$name/unreproduced"
        local _unrep_f="$_unrep_dir/$_mid"
        if _repro_is_red "$suites_csv" "$repo" "spira/$_mid"; then
            ejected+=("$_mm")
            _any_suite_in_selection "$red_suites" "$repo" "$base_sha" "$_mtip" \
                && caught=$(( caught + 1 )) || escaped=$(( escaped + 1 ))
        else
            local _prev_tip; _prev_tip="$(cat "$_unrep_f" 2>/dev/null || true)"
            if [ "${_prev_tip:-}" = "$_mtip" ]; then
                # Second unreproduced red on same tip → eject.
                ejected+=("$_mm")
                escaped=$(( escaped + 1 ))
                rm -f "$_unrep_f" 2>/dev/null || true
            else
                # First unreproduced red → record, quarantine suites, requeue.
                mkdir -p "$_unrep_dir"
                printf '%s\n' "$_mtip" > "$_unrep_f"
                _quarantine_suites "$red_suites"
                survivors+=("$_mm")
            fi
        fi
    else
        # Per-member reproduction.
        for _mm in "${members_arr[@]}"; do
            _mid="${_mm%%:*}"; _mtip="${_mm##*:}"
            if _repro_is_red "$suites_csv" "$repo" "spira/$_mid"; then
                ejected+=("$_mm")
                _any_suite_in_selection "$red_suites" "$repo" "$base_sha" "$_mtip" \
                    && caught=$(( caught + 1 )) || escaped=$(( escaped + 1 ))
            fi
        done

        if [ "${#ejected[@]}" -eq 0 ]; then
            # None reproduced individually — test the batch head.
            if _repro_is_red "$suites_csv" "$repo" "$branch_name"; then
                # Together-only break → halve: first half gets epoch=1 (batches immediately),
                # second half gets epoch=now (waits for BATCH_WAIT).
                local half=$(( mc / 2 )) i=0
                for _mm in "${members_arr[@]}"; do
                    _mid="${_mm%%:*}"; _mtip="${_mm##*:}"
                    if [ "$i" -lt "$half" ]; then
                        land_mark_at "$_mid" CERTIFIED "$_mtip" 1
                    else
                        land_mark "$_mid" CERTIFIED "$_mtip"
                    fi
                    i=$(( i + 1 ))
                done
                "$forge" pr-close "$repo" "$pr_n" 2>/dev/null || true
                rm -f "$batch_file"
                printf 'verdict %s: PR %s together-only red — halved (%d+%d)\n' \
                    "$name" "$pr_n" "$half" "$(( mc - half ))"
                _meter_write "$name" "$mc" 0 0 "$attr_start"
                return 0
            else
                # Flake — quarantine suites, requeue all.
                _quarantine_suites "$red_suites"
                for _mm in "${members_arr[@]}"; do survivors+=("$_mm"); done
            fi
        else
            # Some reproduced — collect non-ejected survivors.
            local ejected_set=" $(printf '%s\n' "${ejected[@]}" | sed 's/:.*//' | tr '\n' ' ')"
            for _mm in "${members_arr[@]}"; do
                _mid="${_mm%%:*}"
                case "$ejected_set" in *" $_mid "*) ;; *) survivors+=("$_mm") ;; esac
            done
        fi
    fi

    # Eject guilty members.
    for _mm in "${ejected[@]}"; do
        _mid="${_mm%%:*}"; _mtip="${_mm##*:}"
        _attr_eject "$_mid" "$_mtip" "$suites_csv" "$pr_n" "$name"
    done

    # Return survivors to CERTIFIED.
    for _mm in "${survivors[@]}"; do
        _mid="${_mm%%:*}"; _mtip="${_mm##*:}"
        land_mark "$_mid" CERTIFIED "$_mtip"
    done

    if [ "${#ejected[@]}" -gt 0 ] || [ "${#survivors[@]}" -gt 0 ]; then
        "$forge" pr-close "$repo" "$pr_n" 2>/dev/null || true
        rm -f "$batch_file"
        printf 'verdict %s: PR %s — ejected %d, requeued %d\n' \
            "$name" "$pr_n" "${#ejected[@]}" "${#survivors[@]}"
    fi

    _meter_write "$name" "$mc" "$caught" "$escaped" "$attr_start"
}

main() {
    local name="${1:-}"
    [ -n "$name" ] || { printf 'verdict.sh: repo name required\n' >&2; exit 1; }

    local repo mode
    repo="$(repo_root "$name")" || {
        printf 'verdict %s: no repo-map entry\n' "$name" >&2
        return 1
    }
    mode="$(repo_land "$name")"
    [ "$mode" = "queue" ] || return 0

    local batch_file
    batch_file="$(_batch_open_file "$name")"
    [ -f "$batch_file" ] || return 0

    local pr_n batch_head base_sha members_str opened run_retries branch_name
    pr_n="$(_batch_field pr "$batch_file")"
    batch_head="$(_batch_field head "$batch_file")"
    base_sha="$(_batch_field base "$batch_file")"
    members_str="$(_batch_field members "$batch_file")"
    opened="$(_batch_field opened "$batch_file")"
    branch_name="$(_batch_field branch "$batch_file")"
    run_retries="$(_batch_field retries "$batch_file")"
    run_retries="${run_retries:-0}"

    [ -n "$pr_n" ] && [ -n "$batch_head" ] && [ -n "$base_sha" ] || {
        printf 'verdict %s: malformed batch record\n' "$name" >&2
        return 1
    }

    local base remote base_branch
    base="$(spira_landref "$repo")" || {
        printf 'verdict %s: cannot resolve base ref\n' "$name" >&2
        return 1
    }
    remote="$(ref_remote "$base")"
    base_branch="$(ref_branch "$base")"

    local forge="${SPIRA_FORGE:-$HERE/forge.sh}"

    local status_out status
    status_out="$("$forge" check-status "$repo" "$pr_n" 2>/dev/null)" || status_out="pending"
    status="$(printf '%s\n' "$status_out" | head -1)"
    status="${status:-pending}"

    local now; now="$(date +%s)"

    case "$status" in
        pending)
            local age=$(( now - ${opened:-0} ))
            if [ "$age" -lt "${SPIRA_QUEUE_CI_MAXSEC:-3600}" ]; then
                printf 'verdict %s: PR %s pending (%ds old)\n' "$name" "$pr_n" "$age"
                return 0
            fi
            printf 'verdict %s: PR %s pending too long (%ds) — treating as harness fault\n' \
                "$name" "$pr_n" "$age"
            status="harness_fault"
            ;&
        harness_fault)
            local max_retries="${SPIRA_QUEUE_INFRA_RETRIES:-2}"
            if [ "$run_retries" -lt "$max_retries" ]; then
                local run_id
                run_id="$("$forge" run-id "$repo" "${branch_name:-}" 2>/dev/null)" || run_id=""
                if [ -n "${run_id:-}" ]; then
                    "$forge" workflow-rerun "$repo" "$run_id" 2>/dev/null || true
                fi
                printf 'verdict %s: PR %s harness fault — re-running (attempt %d/%d)\n' \
                    "$name" "$pr_n" "$(( run_retries + 1 ))" "$max_retries"
                _batch_set_retries "$batch_file" "$(( run_retries + 1 ))"
            else
                printf 'verdict %s: PR %s harness fault — retries exhausted; mailing operator\n' \
                    "$name" "$pr_n"
                printf '## Note\nMerge queue batch for %s has not been tested after %d CI run attempts.\n\nPR %s (head %s) cannot advance until the CI issue is resolved.\n' \
                    "$name" "$(( run_retries + 1 ))" "$pr_n" "$batch_head" \
                | bash "$HERE/mail.sh" send operator \
                    --from "Spira Queue <queue@spira>" \
                    --subject "Merge queue: $name CI fault after $(( run_retries + 1 )) attempts" \
                    2>/dev/null || true
            fi
            ;;
        green)
            local current_base
            current_base="$(git -C "$repo" rev-parse "$base" 2>/dev/null)" || current_base=""

            if [ "$current_base" = "$base_sha" ]; then
                if git -C "$repo" push "$remote" "${batch_head}:${base_branch}" 2>/dev/null; then
                    printf 'verdict %s: PR %s landed by fast-forward (%s)\n' \
                        "$name" "$pr_n" "$batch_head"
                    local _mm _mid _mtip
                    for _mm in $members_str; do
                        _mid="${_mm%%:*}"; _mtip="${_mm##*:}"
                        land_mark "$_mid" LANDED "$_mtip"
                    done
                    while IFS= read -r _line; do
                        case "$_line" in
                            "flaky: "*)
                                bash "$HERE/suites.sh" observe-flake "${_line#flaky: }" \
                                    2>/dev/null || true
                                ;;
                        esac
                    done <<< "$status_out"
                    rm -f "$batch_file"
                    # Delete the batch branch now that the base has advanced past it.
                    if [ -n "${branch_name:-}" ]; then
                        git -C "$repo" push -q "$remote" --delete "$branch_name" 2>/dev/null || true
                        git -C "$repo" branch -D "$branch_name" 2>/dev/null || true
                        printf 'verdict %s: deleted batch branch %s\n' "$name" "$branch_name"
                    fi
                else
                    printf 'verdict %s: PR %s fast-forward push failed\n' "$name" "$pr_n" >&2
                    return 1
                fi
            else
                "$forge" pr-close "$repo" "$pr_n" 2>/dev/null || true
                printf 'verdict %s: PR %s base moved (%s) — closed, members requeued\n' \
                    "$name" "$pr_n" "${current_base:-unknown}"
                local _mm _mid _mtip
                for _mm in $members_str; do
                    _mid="${_mm%%:*}"; _mtip="${_mm##*:}"
                    land_mark "$_mid" CERTIFIED "$_mtip"
                done
                rm -f "$batch_file"
            fi
            ;;
        red)
            _q_attribute "$name" "$repo" "$pr_n" "$batch_file" "$branch_name" \
                "$batch_head" "$base_sha" "$forge" "$status_out" "$members_str"
            ;;
        *)
            printf 'verdict %s: PR %s unknown check status: %s\n' "$name" "$pr_n" "$status" >&2
            ;;
    esac
}

main "$@"
