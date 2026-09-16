#!/usr/bin/env bash
# verdict.sh — merge-queue verdict: read CI result, fast-forward or handle faults.
# Usage: verdict.sh <repo-name>
#
# Reads the open batch file for a queue-mode repository. Checks the PR's gate
# check status through the forge seam. Green with an unchanged base fast-forwards
# the land ref and marks every member LANDED. A moved base closes the PR and
# returns members to CERTIFIED. Pending does nothing unless the batch has waited
# longer than SPIRA_QUEUE_CI_MAXSEC. A harness fault re-runs the workflow up to
# SPIRA_QUEUE_INFRA_RETRIES times, then mails the operator. Red is logged;
# attribution is handled separately.
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

_batch_open_file() { printf '%s/%s/open' "${SPIRA_QUEUE_DIR:?}" "$1"; }

_batch_field() {    # _batch_field <key> <file>
    grep "^$1=" "$2" 2>/dev/null | cut -d= -f2-
}

_batch_set_retries() {  # _batch_set_retries <file> <n>
    local f="$1" n="$2"
    { grep -v '^retries=' "$f"; printf 'retries=%s\n' "$n"; } > "$f.$$" \
        && mv -f "$f.$$" "$f"
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
            printf 'verdict %s: PR %s red — attribution pending\n' "$name" "$pr_n"
            ;;
        *)
            printf 'verdict %s: PR %s unknown check status: %s\n' "$name" "$pr_n" "$status" >&2
            ;;
    esac
}

main "$@"
