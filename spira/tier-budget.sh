#!/usr/bin/env bash
#
# tier-budget.sh — per-tier suite wall-time budgets, with a shrink-only allowlist for
# today's violators (law-unit-tests-run-under-a-second; test plan §4.1; sp-5m133).
#
#   tier-budget.sh check <suite> [--suite-dir DIR] [--root RUN_ROOT] [--window N]
#       Trailing-median check for one suite against its tier budget or allowlist entry.
#       Exits 1 naming tier, budget and measured time on a violation; 2 if it cannot judge
#       (no duckdb, no data) — a control that cannot check must refuse, not pass silently.
#
#   tier-budget.sh check-batch --suite-dir DIR --tsv FILE [--root RUN_ROOT] [--window N]
#       Same check for every suite named in FILE (testenv-batch.sh's suite-times.tsv),
#       one duckdb call for the whole batch. Exits 1 naming every violator.
#
#   tier-budget.sh check-areas [--suite-dir DIR]
#       T0: at most one T3 suite per area (test plan §4.1). An area is the <area> in a
#       suite's `UC-<area>-NN` covers: token; a T3 suite naming no UC id belongs to no area
#       and is never counted. Exits 1 naming every area over its cap and the suites in it,
#       unless the excess is grandfathered in SPIRA_TIER_AREA_ALLOWLIST.
#
#   tier-budget.sh lint-allowlist [--base REF] [--prior FILE] [--file PATH]
#       T0: the checked-in allowlist may only shrink. Fails if any entry is new or any
#       recorded value is higher than at REF (default: spira_landref). Missing REF or no
#       allowlist there is treated as an empty prior state — permitted only because that is
#       exactly the shape of this file's own introducing commit. --prior FILE reads the prior
#       state from FILE instead of `git show REF:path` — a stand-in for tests, the same role
#       broker-allowlist-lint.sh's --scan-conf plays for conf.sh. --file PATH lints a ledger
#       other than SPIRA_TIER_ALLOWLIST — SPIRA_TIER_AREA_ALLOWLIST is shaped the same way
#       (key in column 1, value in the last column) so the same shrink-only rule reads either.
#
# BUDGETS (ms): T0/T1 SPIRA_TIER_BUDGET_T0_MS/T1_MS (default 1000), T2 …T2_MS (10000),
# T3 …T3_MS (60000). An untagged suite counts as T1 (never as "no budget").
#
# ALLOWLIST: SPIRA_TIER_ALLOWLIST, tab-separated `<suite> <tier> <seconds>`. An allowlisted
# suite's budget is its recorded seconds * (1 + SPIRA_TIER_ALLOWLIST_MARGIN_PCT/100), which
# replaces the tier budget rather than adding to it — the whole point of grandfathering a
# violator in is that its tier budget alone would already fail it.
#
# AREA ALLOWLIST: SPIRA_TIER_AREA_ALLOWLIST, tab-separated `<area> <count>` — the same
# ratchet as above, one T3-per-area count instead of one suite's wall time, so today's
# multi-suite areas don't block every branch until their suites are consolidated elsewhere.
#
# MEASUREMENT: a DuckDB trailing median over the suite's last SPIRA_TIER_BUDGET_WINDOW rows
# in run/tsd/suite-timing.jsonl (sp-sbc6o), which absorbs load noise better than comparing
# one sample. The row tsd_suite_timing() in testenv-batch.sh writes for THIS run is expected
# to already be there by the time this runs, so a suite with no prior history still gets a
# median — of one.
#
# covers: spira/testenv-batch.sh spira/gate-touched.sh spira/suite-covers.sh spira/tier-budget-allowlist spira/tier-budget-area-allowlist spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"
. "$HERE/suite-covers.sh"

SUITE_RE='^[A-Za-z0-9][A-Za-z0-9._-]*$'
TIER_RE='^T[0-3]$'

_tier_budget_ms() {  # _tier_budget_ms <tier> -> budget in ms
    case "$1" in
        T0) printf '%s' "${SPIRA_TIER_BUDGET_T0_MS:-1000}" ;;
        T1) printf '%s' "${SPIRA_TIER_BUDGET_T1_MS:-1000}" ;;
        T2) printf '%s' "${SPIRA_TIER_BUDGET_T2_MS:-10000}" ;;
        T3) printf '%s' "${SPIRA_TIER_BUDGET_T3_MS:-60000}" ;;
        *)  printf '%s' "${SPIRA_TIER_BUDGET_T1_MS:-1000}" ;;
    esac
}

# _suite_tier <suite-dir> <suite> -> "T0".."T3"; untagged or unknown counts as T1.
_suite_tier() {
    local t
    t="$(suite_tier_of "$1/$2" 2>/dev/null || true)"
    [[ "$t" =~ $TIER_RE ]] && printf '%s' "$t" || printf 'T1'
}

# _allowlist_secs <suite> -> the recorded seconds, or empty if not allowlisted.
_allowlist_secs() {
    local f="${SPIRA_TIER_ALLOWLIST:-}"
    [ -r "$f" ] || return 0
    awk -F'\t' -v s="$1" '!/^[[:space:]]*#/ && !/^[[:space:]]*$/ && $1==s {print $3; exit}' "$f"
}

# _budget_secs <suite> <tier> -> the seconds this suite must stay under right now.
_budget_secs() {
    local suite="$1" tier="$2" allow
    allow="$(_allowlist_secs "$suite")"
    if [ -n "$allow" ]; then
        awk -v a="$allow" -v m="${SPIRA_TIER_ALLOWLIST_MARGIN_PCT:-20}" \
            'BEGIN{printf "%.3f", a * (1 + m/100)}'
        return 0
    fi
    awk -v ms="$(_tier_budget_ms "$tier")" 'BEGIN{printf "%.3f", ms/1000}'
}

_family_path() { printf '%s/tsd/suite-timing.jsonl' "${1:-$SPIRA_RUN}"; }

# _trailing_medians <root> <window> -> "<suite>\t<median>" one per line, over every suite
# with at least one row. One duckdb call regardless of how many suites are being judged.
_trailing_medians() {
    local path window="$2"
    path="$(_family_path "$1")"
    [ -f "$path" ] || return 0
    duckdb -json -c "
        WITH ranked AS (
            SELECT suite, wall_secs,
                   row_number() OVER (PARTITION BY suite ORDER BY ts DESC) AS rn
            FROM read_ndjson_auto('$path')
        )
        SELECT suite, median(wall_secs) AS m FROM ranked WHERE rn <= $window GROUP BY suite;
    " 2>/dev/null | python3 -c '
import json, sys
for row in json.load(sys.stdin):
    print(str(row["suite"]) + "\t" + str(row["m"]))
' 2>/dev/null
}

# _judge <suite> <tier> <median> -> prints a violation line and returns 1, or returns 0.
_judge() {
    local suite="$1" tier="$2" median="$3" budget
    budget="$(_budget_secs "$suite" "$tier")"
    awk -v m="$median" -v b="$budget" 'BEGIN{exit !(m>b)}' || return 0
    printf 'tier-budget: %s tier=%s budget=%ss measured=%ss — over budget\n' \
        "$suite" "$tier" "$budget" "$median" >&2
    return 1
}

cmd_check() {
    local suite="" suite_dir="$HERE" root="${SPIRA_RUN:-}" window="${SPIRA_TIER_BUDGET_WINDOW:-20}"
    suite="${1:?suite required}"; shift || true
    while [ $# -gt 0 ]; do
        case "$1" in
            --suite-dir) suite_dir="$2"; shift 2 ;;
            --root)      root="$2"; shift 2 ;;
            --window)    window="$2"; shift 2 ;;
            *) printf 'tier-budget: unknown arg: %s\n' "$1" >&2; return 2 ;;
        esac
    done
    [[ "$suite" =~ $SUITE_RE ]] || { printf 'tier-budget: bad suite name %q\n' "$suite" >&2; return 2; }
    spira_require duckdb python3 || return 2
    local tier medians med
    tier="$(_suite_tier "$suite_dir" "$suite")"
    medians="$(_trailing_medians "$root" "$window")"
    med="$(printf '%s\n' "$medians" | awk -F'\t' -v s="$suite" '$1==s{print $2; exit}')"
    if [ -z "$med" ]; then
        printf 'tier-budget: no tsd suite-timing rows for %s — cannot judge\n' "$suite" >&2
        return 2
    fi
    _judge "$suite" "$tier" "$med"
}

cmd_check_batch() {
    local suite_dir="$HERE" root="${SPIRA_RUN:-}" window="${SPIRA_TIER_BUDGET_WINDOW:-20}" tsv=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --suite-dir) suite_dir="$2"; shift 2 ;;
            --root)      root="$2"; shift 2 ;;
            --window)    window="$2"; shift 2 ;;
            --tsv)       tsv="$2"; shift 2 ;;
            *) printf 'tier-budget: unknown arg: %s\n' "$1" >&2; return 2 ;;
        esac
    done
    [ -r "$tsv" ] || { printf 'tier-budget: no such tsv: %s\n' "$tsv" >&2; return 2; }
    spira_require duckdb python3 || return 2
    local medians bad=0 suite tier med
    medians="$(_trailing_medians "$root" "$window")"
    while IFS=$'\t' read -r _run _branch suite _rc _wall _bdc _bdms _mode; do
        [ -n "$suite" ] || continue
        [ "$suite" = "__batch__" ] && continue
        [[ "$suite" =~ $SUITE_RE ]] || continue
        tier="$(_suite_tier "$suite_dir" "$suite")"
        med="$(printf '%s\n' "$medians" | awk -F'\t' -v s="$suite" '$1==s{print $2; exit}')"
        [ -n "$med" ] || continue
        _judge "$suite" "$tier" "$med" || bad=1
    done < "$tsv"
    [ "$bad" = 0 ]
}

# _t3_area_rows <suite-dir> -> "<area>\t<suite>" one per line, for every UC area named by a
# T3 suite. A T3 suite with no UC id on its # covers: line contributes no row. A suite naming
# the same area via more than one UC id (e.g. UC-operator-channel-14 and -15 on one line)
# contributes exactly one row for it — sort -u collapses the rest, so a multi-UC suite is
# still one suite when an area's T3 count is judged.
_t3_area_rows() {
    local dir="$1" f tier uc area
    {
        for f in "$dir"/test-*.sh; do
            [ -r "$f" ] || continue
            tier="$(suite_tier_of "$f" 2>/dev/null || true)"
            [ "$tier" = T3 ] || continue
            for uc in $(suite_uc_of "$f" 2>/dev/null || true); do
                area="${uc#UC-}"; area="${area%-*}"
                [ -n "$area" ] || continue
                printf '%s\t%s\n' "$area" "$(basename "$f")"
            done
        done
    } | sort -u
}

# _area_allowlist_count <area> -> the recorded T3 count, or empty if not allowlisted.
_area_allowlist_count() {
    local f="${SPIRA_TIER_AREA_ALLOWLIST:-}"
    [ -r "$f" ] || return 0
    awk -F'\t' -v a="$1" '!/^[[:space:]]*#/ && !/^[[:space:]]*$/ && $1==a {print $NF; exit}' "$f"
}

cmd_check_areas() {
    local suite_dir="$HERE"
    while [ $# -gt 0 ]; do
        case "$1" in
            --suite-dir) suite_dir="$2"; shift 2 ;;
            *) printf 'tier-budget: unknown arg: %s\n' "$1" >&2; return 2 ;;
        esac
    done
    local rows bad=0 area count allow suites_here
    rows="$(_t3_area_rows "$suite_dir")"
    [ -n "$rows" ] || return 0
    while IFS= read -r area; do
        [ -n "$area" ] || continue
        suites_here="$(printf '%s\n' "$rows" | awk -F'\t' -v a="$area" '$1==a{print $2}')"
        count="$(printf '%s\n' "$suites_here" | grep -c .)"
        allow="$(_area_allowlist_count "$area")"; allow="${allow:-0}"
        if [ "$count" -gt 1 ] && [ "$count" -gt "$allow" ]; then
            printf 'tier-budget: area %s has %d T3 suites (max 1; allowlisted %s) — %s\n' \
                "$area" "$count" "$allow" "$(printf '%s\n' "$suites_here" | paste -sd, -)" >&2
            bad=1
        fi
    done < <(printf '%s\n' "$rows" | cut -f1 | sort -u)
    [ "$bad" = 0 ]
}

cmd_lint_allowlist() {
    local base="" prior_file="" allowlist="${SPIRA_TIER_ALLOWLIST:-}"
    while [ $# -gt 0 ]; do
        case "$1" in
            --base)   base="$2"; shift 2 ;;
            --prior)  prior_file="$2"; shift 2 ;;  # stand-in for `git show base:path`, for tests
            --file)   allowlist="$2"; shift 2 ;;   # lint a different shrink-only ledger, path given directly (tests)
            # --area resolves SPIRA_TIER_AREA_ALLOWLIST inside THIS process (which has sourced
            # conf.sh via lib.sh), not in the caller's — gate-touched.sh never sources conf.sh
            # itself, so a caller-side env lookup would always read empty.
            --area)   allowlist="${SPIRA_TIER_AREA_ALLOWLIST:-}"; shift ;;
            *) printf 'tier-budget: unknown arg: %s\n' "$1" >&2; return 2 ;;
        esac
    done
    [ -r "$allowlist" ] || { printf 'tier-budget: no allowlist at %s\n' "$allowlist" >&2; return 2; }

    # prior_found distinguishes "no allowlist existed at the base at all" (this file's own
    # introducing commit — nothing to compare against, not a violation) from "an allowlist
    # existed there but does not mention this suite" (a genuine new entry). Conflating the
    # two would fail this file's own seed commit on every one of its lines.
    local prior="" prior_found=0
    if [ -n "$prior_file" ]; then
        if [ -r "$prior_file" ]; then prior="$(cat "$prior_file")"; prior_found=1; fi
    else
        [ -n "$base" ] || base="$(spira_landref 2>/dev/null || true)"
        local relpath repo_root
        repo_root="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null || true)"
        if [ -n "$repo_root" ]; then
            relpath="${allowlist#"$repo_root"/}"
        else
            relpath="spira/$(basename "$allowlist")"
        fi
        if [ -n "$base" ] && [ -n "$repo_root" ]; then
            if prior="$(git -C "$repo_root" show "$base:$relpath" 2>/dev/null)"; then
                prior_found=1
            fi
        fi
    fi
    if [ "$prior_found" = 0 ]; then
        printf 'tier-budget: lint-allowlist: no prior allowlist found — treating this as its introducing commit\n' >&2
        return 0
    fi

    # KEY is column 1, VALUE is the LAST column — true for both the suite allowlist
    # (<suite> <tier> <seconds>) and the area allowlist (<area> <count>), so one loop
    # lints either ledger without caring which one it was handed.
    local bad=0
    while IFS=$'\t' read -r key val; do
        case "$key" in ''|'#'*) continue ;; esac
        [ -n "$key" ] || continue
        local prior_val
        prior_val="$(printf '%s\n' "$prior" | \
            awk -F'\t' -v k="$key" '!/^[[:space:]]*#/ && $1==k {print $NF; exit}')"
        if [ -z "$prior_val" ]; then
            printf 'tier-budget: lint-allowlist: new entry %s — the allowlist may only shrink\n' "$key" >&2
            bad=1
            continue
        fi
        if awk -v now="$val" -v was="$prior_val" 'BEGIN{exit !(now>was)}'; then
            printf 'tier-budget: lint-allowlist: %s raised %s -> %s — the allowlist may only shrink\n' \
                "$key" "$prior_val" "$val" >&2
            bad=1
        fi
    done < <(grep -v '^[[:space:]]*#' "$allowlist" | grep -v '^[[:space:]]*$' | awk -F'\t' '{print $1"\t"$NF}')
    [ "$bad" = 0 ]
}

usage() {
    sed -n '2,47p' "$0" | sed 's/^# \{0,1\}//'
}

case "${1:-}" in
    check)          shift; cmd_check "$@" ;;
    check-batch)    shift; cmd_check_batch "$@" ;;
    check-areas)    shift; cmd_check_areas "$@" ;;
    lint-allowlist) shift; cmd_lint_allowlist "$@" ;;
    --help|-h|'')   usage; exit 0 ;;
    *) printf 'tier-budget.sh: unknown command: %s\n' "$1" >&2; usage >&2; exit 2 ;;
esac
