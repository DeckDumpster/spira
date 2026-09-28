#!/usr/bin/env bash
# attribute.sh — local, parallel red-suite attribution with bisection.
#
# A round's local corpus went red on some suites; name the member responsible within
# minutes, on this box, by re-running ONLY those suites through testenv-batch.sh
# (law-tests-run-only-through-testenv-batch), never anything CI would otherwise wait an
# hour for. verdict.sh's own attribution (_q_attribute) dispatches reproduction to a
# member's own CI run, a round trip this box does not need to pay: every member's
# certified tip already lives on this repo as refs/heads/spira/<id>, so the tree to test
# is one local merge away.
#
# METHOD, per suite:
#   base    — red on <base> alone (no member merged in). Never blamed on a member.
#   single  — some member's tip, merged onto <base> alone, reproduces it.
#   bisect  — no single member reproduces it, but the full round does: halve the
#             member list, test each half (in parallel), recurse into whichever half
#             still reproduces it; if neither half alone does, the current group is
#             the minimal interaction that does (queue_bisect_split's halving
#             convention: first half no larger than the second).
#
# USAGE
#   attribute.sh --round <round-branch> --base <ref> --suites <a,b,c>
#                --members <id,id,...> [--repo <name-or-path>]
#
# OUTPUT
#   One line per red suite:
#     ATTR <suite> owner=<id[,id...]|BASE> method=<base|single|bisect> wall=<secs>s fail=<line>
#   Then one summary line per member with anything to fix:
#     EJECT <id> <suite[,suite...]>
#
# ENVIRONMENT
#   SPIRA_ATTRIBUTE_MAXPAR  max concurrent testenv-batch.sh runs (default: 32 — the host's
#                           core count, not SPIRA_AEON_CPU_QUOTA: attribution is a one-shot
#                           local burst, not a share of an aeon's steady-state budget).
#
# covers: spira/attribute.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"

# ---------------------------------------------------------------------------
# ARGS
# ---------------------------------------------------------------------------
ROUND="" BASE="" SUITES_CSV="" MEMBERS_CSV="" REPO_ARG="" BATCH_ID=""
while [ $# -gt 0 ]; do
    case "$1" in
        --round)   [ $# -ge 2 ] || { printf 'attribute: --round requires an argument\n' >&2; exit 2; }
                   ROUND="$2"; shift 2 ;;
        --base)    [ $# -ge 2 ] || { printf 'attribute: --base requires an argument\n' >&2; exit 2; }
                   BASE="$2"; shift 2 ;;
        --suites)  [ $# -ge 2 ] || { printf 'attribute: --suites requires an argument\n' >&2; exit 2; }
                   SUITES_CSV="$2"; shift 2 ;;
        --members) [ $# -ge 2 ] || { printf 'attribute: --members requires an argument\n' >&2; exit 2; }
                   MEMBERS_CSV="$2"; shift 2 ;;
        --repo)    [ $# -ge 2 ] || { printf 'attribute: --repo requires an argument\n' >&2; exit 2; }
                   REPO_ARG="$2"; shift 2 ;;
        # --batch-id: the round's lifecycle batch id (queue.sh's `batch_id=` on its open
        # record), so this pass's timing lands as one round row (run/tsd/, sp-69m85) keyed
        # to the same batch every other phase uses. Optional — omitted, no row is written;
        # attribution itself is unaffected either way.
        --batch-id) [ $# -ge 2 ] || { printf 'attribute: --batch-id requires an argument\n' >&2; exit 2; }
                   BATCH_ID="$2"; shift 2 ;;
        -*)        printf 'attribute: unknown option: %s\n' "$1" >&2; exit 2 ;;
        *)         printf 'attribute: unexpected argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
[ -n "$BASE" ] && [ -n "$SUITES_CSV" ] && [ -n "$MEMBERS_CSV" ] || {
    printf 'usage: attribute.sh --round <round-branch> --base <ref> --suites <a,b,c> --members <id,id,...> [--repo <name>]\n' >&2
    exit 2
}

REPO="${REPO_ARG:-${SPIRA_REPO:-}}"
if [ -n "$REPO_ARG" ]; then
    case "$REPO_ARG" in
        */*) REPO="$REPO_ARG" ;;
        *)   REPO="$(repo_root "$REPO_ARG" 2>/dev/null)" || {
                 printf 'attribute: cannot find repo %s in repo-map\n' "$REPO_ARG" >&2
                 exit 2
             } ;;
    esac
elif [ -z "$REPO" ]; then
    REPO="$(cd "$HERE/.." && pwd -P)"
fi

BASE_SHA="$(git -C "$REPO" rev-parse --verify -q "$BASE^{commit}" 2>/dev/null)" || {
    printf 'attribute: cannot resolve --base %s in %s\n' "$BASE" "$REPO" >&2
    exit 2
}

IFS=',' read -ra SUITES_ARR <<< "$SUITES_CSV"
IFS=',' read -ra MEMBERS_ARR <<< "$MEMBERS_CSV"
[ "${#SUITES_ARR[@]}" -gt 0 ] || { printf 'attribute: --suites named nothing\n' >&2; exit 2; }
[ "${#MEMBERS_ARR[@]}" -gt 0 ] || { printf 'attribute: --members named nothing\n' >&2; exit 2; }

MAXPAR="${SPIRA_ATTRIBUTE_MAXPAR:-32}"
_ATTR_PASS_START=$EPOCHSECONDS

_ATTR_WORK="$(mktemp -d "${SPIRA_RUN:-${TMPDIR:-/tmp}}/attribute.XXXXXX")"
trap 'rm -rf "$_ATTR_WORK"' EXIT

# _attr_member_tip <id> -> the member's certified branch tip in $REPO, or non-zero.
_attr_member_tip() {
    git -C "$REPO" rev-parse --verify -q "refs/heads/spira/$1" 2>/dev/null
}

# _attr_build_tree <tag> <member-id...> -> a commit sha (base_sha merged with each
# member's tip, in order, mirroring batcher-cut's own merge_member: sequential
# --no-ff merges in one throwaway worktree), or non-zero on any merge/resolve fault.
_attr_build_tree() {
    local tag="$1"; shift
    [ $# -gt 0 ] || { printf '%s' "$BASE_SHA"; return 0; }
    local wt="$_ATTR_WORK/wt-$tag" id tip
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    git -C "$REPO" worktree add -q --detach "$wt" "$BASE_SHA" 2>/dev/null || return 1
    for id in "$@"; do
        tip="$(_attr_member_tip "$id")" || { git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true; return 1; }
        if ! git -C "$wt" merge -q --no-edit --no-ff -m "attribute: land $id" "$tip" >/dev/null 2>&1; then
            git -C "$wt" merge --abort 2>/dev/null || true
            git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
            return 1
        fi
    done
    local sha; sha="$(git -C "$wt" rev-parse HEAD 2>/dev/null)" || {
        git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
        return 1
    }
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    printf '%s' "$sha"
}

# _attr_run <tag> <members-csv> <suites-csv> -> runs testenv-batch.sh for the tree
# named by <members-csv> (comma list of member ids, empty for base alone) restricted
# to <suites-csv>, writing per-suite status/wall/fail-line under
# $_ATTR_WORK/out-<tag>/<suite>.{status,wall,fail}. Sets _ATTR_RUN_FAULT=1 on a build
# or harness fault (never on a plain red — a red is a real, attributable result).
_ATTR_RUN_FAULT=0
_attr_run() {
    local tag="$1" members_csv="$2" suites_csv="$3"
    local out="$_ATTR_WORK/out-$tag"
    mkdir -p "$out"
    _ATTR_RUN_FAULT=0

    local -a mids=()
    if [ -n "$members_csv" ]; then IFS=',' read -ra mids <<< "$members_csv"; fi
    local sha; sha="$(_attr_build_tree "$tag" "${mids[@]}")" || { _ATTR_RUN_FAULT=1; return 1; }

    local results="$out/results"
    SPIRA_BATCH_RESULTS="$results" SPIRA_VERDICT_TTL=0 \
        bash "$HERE/testenv-batch.sh" --suites "$suites_csv" "$sha" "$REPO" \
        >"$out/log" 2>&1
    local rc=$?
    if [ "$rc" -ge 2 ]; then _ATTR_RUN_FAULT=1; fi

    local resdir; resdir="$(find "$results" -maxdepth 2 -name batch.meta 2>/dev/null | head -1 | xargs -r dirname)"
    local s status wall fline
    IFS=',' read -ra _sarr <<< "$suites_csv"
    for s in "${_sarr[@]}"; do
        [ -n "$s" ] || continue
        if [ -n "$resdir" ] && [ -r "$resdir/$s.result" ]; then
            status="$(awk '{print $1}' "$resdir/$s.result" 2>/dev/null)"
            wall="$(awk '{print $3}' "$resdir/$s.result" 2>/dev/null)"
        else
            status="unreached"; wall=""
        fi
        fline=""
        if [ "$status" != ok ] && [ -n "$resdir" ] && [ -r "$resdir/$s.out" ]; then
            fline="$(grep -m1 '^not ok' "$resdir/$s.out" 2>/dev/null || true)"
        fi
        printf '%s\n' "$status" > "$out/$s.status"
        printf '%s\n' "${wall:-0}" > "$out/$s.wall"
        printf '%s\n' "$fline" > "$out/$s.fail"
    done
    return "$rc"
}

# _attr_suite_red <tag> <suite> -> 0 if that suite's status in <tag>'s output is red.
_attr_suite_red() {
    local out="$_ATTR_WORK/out-$1"
    [ -r "$out/$2.status" ] && [ "$(cat "$out/$2.status" 2>/dev/null)" = red ]
}

# ---------------------------------------------------------------------------
# PARALLEL DRIVER — runs a batch of (tag, members-csv, suites-csv) triples
# concurrently, bounded by SPIRA_ATTRIBUTE_MAXPAR. Each is a full
# _attr_run — build the tree, run testenv-batch.sh — in its own subshell.
# ---------------------------------------------------------------------------
_attr_par_run() {
    local entry tag members suites
    local -a pids=()
    for entry in "$@"; do
        tag="${entry%%|*}"; entry="${entry#*|}"
        members="${entry%%|*}"; suites="${entry#*|}"
        if [ "${MAXPAR:-0}" -gt 0 ] 2>/dev/null; then
            while [ "$(jobs -rp | wc -l)" -ge "$MAXPAR" ]; do wait -n 2>/dev/null || true; done
        fi
        ( _attr_run "$tag" "$members" "$suites" ) &
        pids+=($!)
    done
    wait "${pids[@]}" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# STEP 1 — BASE ALONE. A suite red here predates every member; never blame one.
# ---------------------------------------------------------------------------
log "attribute: base pass (${#SUITES_ARR[@]} suite(s), base ${BASE_SHA:0:12})"
_attr_run base "" "$SUITES_CSV"

declare -A OWNER=() METHOD=() FAIL=() WALL=()
BASE_RED=()
REMAINING=()
for s in "${SUITES_ARR[@]}"; do
    [ -n "$s" ] || continue
    if _attr_suite_red base "$s"; then
        BASE_RED+=("$s")
        OWNER["$s"]="BASE"
        METHOD["$s"]="base"
    else
        REMAINING+=("$s")
    fi
    FAIL["$s"]="$(cat "$_ATTR_WORK/out-base/$s.fail" 2>/dev/null || true)"
    WALL["$s"]="$(cat "$_ATTR_WORK/out-base/$s.wall" 2>/dev/null || echo 0)"
done

# ---------------------------------------------------------------------------
# STEP 2 — PER-MEMBER, IN PARALLEL. One run per member, testing every suite
# still standing after the base pass.
# ---------------------------------------------------------------------------
declare -A SINGLE_OWNERS=()   # suite -> space-separated member ids that alone reproduce it
if [ "${#REMAINING[@]}" -gt 0 ]; then
    _remaining_csv="$(printf '%s,' "${REMAINING[@]}")"
    _remaining_csv="${_remaining_csv%,}"
    log "attribute: single-member pass (${#MEMBERS_ARR[@]} member(s) x ${#REMAINING[@]} suite(s))"
    _jobs=()
    for m in "${MEMBERS_ARR[@]}"; do
        [ -n "$m" ] || continue
        _jobs+=("m-$m|$m|$_remaining_csv")
    done
    _attr_par_run "${_jobs[@]}"
    for m in "${MEMBERS_ARR[@]}"; do
        [ -n "$m" ] || continue
        for s in "${REMAINING[@]}"; do
            if _attr_suite_red "m-$m" "$s"; then
                SINGLE_OWNERS["$s"]="${SINGLE_OWNERS[$s]:-} $m"
                FAIL["$s"]="$(cat "$_ATTR_WORK/out-m-$m/$s.fail" 2>/dev/null || true)"
                WALL["$s"]="$(cat "$_ATTR_WORK/out-m-$m/$s.wall" 2>/dev/null || echo 0)"
            fi
        done
    done
fi

STILL_UNATTRIBUTED=()
for s in "${REMAINING[@]}"; do
    if [ -n "${SINGLE_OWNERS[$s]:-}" ]; then
        OWNER["$s"]="$(printf '%s' "${SINGLE_OWNERS[$s]}" | sed -e 's/^ *//' -e 's/ /,/g')"
        METHOD["$s"]="single"
    else
        STILL_UNATTRIBUTED+=("$s")
    fi
done

# ---------------------------------------------------------------------------
# STEP 3 — BISECT. A suite red only in combination: halve the member list,
# test each half (parallel), recurse into whichever half still reproduces it.
# Neither half alone red -> the current group is the minimal interaction.
# ---------------------------------------------------------------------------
# _attr_bisect <suite> <member-id...> -> prints the space-separated owner set on
# stdout. Assumes the full <member-id...> list is already known to reproduce
# <suite> together (the caller's premise: the round itself went red on it).
_attr_bisect() {
    local suite="$1"; shift
    local -a group=("$@")
    if [ "${#group[@]}" -le 1 ]; then
        printf '%s\n' "${group[*]}"
        return 0
    fi
    local half=$(( ${#group[@]} / 2 )) i
    local -a a=() b=()
    for i in "${!group[@]}"; do
        if [ "$i" -lt "$half" ]; then a+=("${group[$i]}"); else b+=("${group[$i]}"); fi
    done
    local tagbase="bs-$suite-$$-${RANDOM}"
    local acsv bcsv; acsv="$(printf '%s,' "${a[@]}")"; acsv="${acsv%,}"
    bcsv="$(printf '%s,' "${b[@]}")"; bcsv="${bcsv%,}"
    _attr_par_run "${tagbase}a|$acsv|$suite" "${tagbase}b|$bcsv|$suite"
    local a_red=1 b_red=1
    _attr_suite_red "${tagbase}a" "$suite" && a_red=0
    _attr_suite_red "${tagbase}b" "$suite" && b_red=0
    if [ "$a_red" -eq 0 ] && [ "$b_red" -eq 0 ]; then
        _attr_bisect "$suite" "${a[@]}"
        _attr_bisect "$suite" "${b[@]}"
    elif [ "$a_red" -eq 0 ]; then
        FAIL["$suite"]="$(cat "$_ATTR_WORK/out-${tagbase}a/$suite.fail" 2>/dev/null || true)"
        WALL["$suite"]="$(cat "$_ATTR_WORK/out-${tagbase}a/$suite.wall" 2>/dev/null || echo 0)"
        _attr_bisect "$suite" "${a[@]}"
    elif [ "$b_red" -eq 0 ]; then
        FAIL["$suite"]="$(cat "$_ATTR_WORK/out-${tagbase}b/$suite.fail" 2>/dev/null || true)"
        WALL["$suite"]="$(cat "$_ATTR_WORK/out-${tagbase}b/$suite.wall" 2>/dev/null || echo 0)"
        _attr_bisect "$suite" "${b[@]}"
    else
        printf '%s\n' "${group[*]}"
    fi
}

for s in "${STILL_UNATTRIBUTED[@]}"; do
    log "attribute: bisecting $s (${#MEMBERS_ARR[@]} member(s))"
    owners="$(_attr_bisect "$s" "${MEMBERS_ARR[@]}" | tail -1)"
    OWNER["$s"]="$(printf '%s' "$owners" | sed -e 's/^ *//' -e 's/ /,/g')"
    METHOD["$s"]="bisect"
done

# ---------------------------------------------------------------------------
# OUTPUT
# ---------------------------------------------------------------------------
declare -A EJECT_SUITES=()
for s in "${SUITES_ARR[@]}"; do
    [ -n "$s" ] || continue
    printf 'ATTR %s owner=%s method=%s wall=%ss fail=%s\n' \
        "$s" "${OWNER[$s]:-UNKNOWN}" "${METHOD[$s]:-unknown}" "${WALL[$s]:-0}" "${FAIL[$s]:-}"
    if [ "${OWNER[$s]:-}" != BASE ] && [ -n "${OWNER[$s]:-}" ]; then
        IFS=',' read -ra _owners <<< "${OWNER[$s]}"
        for o in "${_owners[@]}"; do
            [ -n "$o" ] || continue
            EJECT_SUITES["$o"]="${EJECT_SUITES[$o]:-}${EJECT_SUITES[$o]:+,}$s"
        done
    fi
done
for m in "${MEMBERS_ARR[@]}"; do
    [ -n "${EJECT_SUITES[$m]:-}" ] && printf 'EJECT %s %s\n' "$m" "${EJECT_SUITES[$m]}"
done

# round: TIMINGS ONLY (run/tsd/, sp-69m85) — this pass's own wall time, never a verdict;
# the round's state stays the lifecycle machine's alone (design non-goal, §2a).
[ -n "$BATCH_ID" ] && _tsd_round_phase "$BATCH_ID" attribute \
    "$(( EPOCHSECONDS - _ATTR_PASS_START ))" "${#MEMBERS_ARR[@]}" "${#SUITES_ARR[@]}"

[ "${#BASE_RED[@]}" -eq 0 ] && exit 0 || exit 0
