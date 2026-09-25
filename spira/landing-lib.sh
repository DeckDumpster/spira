#!/usr/bin/env bash
# Sourced, never executed.
# landing-lib.sh — the pure decision seams land_repo (landing.sh) depends on, split out so
# a T1 suite can call them directly instead of paying for a full landing pass (testdb,
# worktrees, a gate stub, a queue) to exercise one branch of one decision.
#
# NOTHING HERE READS A BEAD, A GIT TREE OR THE CLOCK ON ITS OWN ACCOUNT. Each function
# takes what it needs as an argument — a row of already-fetched bead fields, a landstate
# line already read, a gate transcript already captured — so sourcing this file has no
# side effect and calling a function needs no bd, no git repository, no lock. land_repo
# supplies the bd- and git-facing glue and passes the results in.
#
# Sourced, never executed:
#   . "$(dirname "$0")/landing-lib.sh"
set -u

# certify_order <name> — reads TSV rows on stdin, one per branch:
#   id  branch  priority  closed_at  external_ref  express(0|1)
# and prints the branch column in certification order.
#
# THREE BUCKETS, EACH SORTED (priority ASC, closed_at ASC): a base-fix branch
# (external_ref=basefail:<name>:*) first — a budget cut must never defer the one fix that
# unblocks every other held branch; an express-labelled branch second; everything else
# last. This is the ordering land_repo used to build in two passes (a fix/non-fix sort,
# then an express/tail partition of what came out) — collapsed here to the one pass that
# produces the same order, because the second pass never re-sorted, only partitioned.
certify_order() {
    local _name="$1" _id _br _pri _cat _extref _expr _bucket
    {
        while IFS=$'\t' read -r _id _br _pri _cat _extref _expr; do
            [ -n "${_id:-}" ] || continue
            case "$_extref" in
                "basefail:$_name:"*) _bucket=0 ;;
                *) [ "${_expr:-0}" = 1 ] && _bucket=1 || _bucket=2 ;;
            esac
            printf '%s\t%s\t%s\t%s\n' "$_bucket" "${_pri:-9999}" "${_cat:-9999-99-99}" "$_br"
        done
    } | sort -t $'\t' -k1,1n -k2,2n -k3,3 | cut -f4
}

# certify_tier <ls_status> <ls_tip> <ls_at> <ls_reason> <cur_tip> <base_ct> — classify one
# closed branch for phase-2 parallel certification. Prints "tier=<0|1|2|skip> reason=<tag>".
#
#   tier 0 (never-gated or promoted to it): no landstate record, or a RED record that no
#     longer describes the current pair — its tip moved, or its reason is
#     conflicts-with-base and the base has advanced since the record was written (that
#     record is a statement about a pair; either side moving makes it stale).
#   tier 1: a non-RED landstate record (e.g. GATED, CONTENT) — gate again, ordinarily.
#   tier 2: a RED record that is current but whose reason is base-red — the base itself
#     may have recovered, so it is still worth a re-gate, just last.
#   skip: a RED record that is current and not base-red — the same tip and base would
#     produce the same result, so the slot is better spent on a branch that was never
#     tried.
#
# base_ct may be empty when the caller has not computed it (only the conflicts-with-base
# row needs it); every other row ignores it.
certify_tier() {
    local st="$1" ls_tip="$2" ls_at="$3" reason="$4" tip="$5" base_ct="$6"
    if [ -z "${st:-}" ]; then
        printf 'tier=0 reason=never-gated\n'; return 0
    fi
    if [ "$st" = RED ] && [ "${reason:-}" = "conflicts-with-base" ]; then
        if [ -n "${ls_at:-}" ] && [ -n "${base_ct:-}" ] && [ "${base_ct:-0}" -gt "${ls_at:-0}" ] 2>/dev/null; then
            printf 'tier=0 reason=cwb-stale-base\n'; return 0
        elif [ "${ls_tip:-}" != "$tip" ]; then
            printf 'tier=0 reason=tip-stale\n'; return 0
        fi
        printf 'tier=skip reason=cwb-current\n'; return 0
    fi
    if [ "$st" = RED ] && [ "${ls_tip:-}" != "$tip" ]; then
        printf 'tier=0 reason=tip-stale\n'; return 0
    fi
    if [ "$st" = RED ] && [ "${ls_tip:-}" = "$tip" ] && [ "${reason:-}" != "base-red" ]; then
        printf 'tier=skip reason=tip-current\n'; return 0
    fi
    if [ "$st" = RED ]; then
        printf 'tier=2 reason=red\n'; return 0
    fi
    printf 'tier=1 reason=non-red\n'
}

# certify_needs_gate <enabled 0|1> <cert_queue_count> <ci_active_count> — prints "skip" when
# a queue-mode pass should certify a closed branch directly, without a local gate, because
# it would be the sole batch member anyway (the bisect argument for per-branch
# certification is vacuous at batch size 1) AND CI is not already busy running something
# else that would make the skip a wasted local gate avoided for no reason. Prints "gate" in
# every other case, including when the feature is disabled (enabled=0).
#
# Callers still re-confirm the bead is closed before certifying on a "skip" — that check
# reads live bd state and does not belong in a pure decision over already-fetched counts.
certify_needs_gate() {
    local en="${1:-1}" cq="${2:-1}" ci="${3:-1}"
    if [ "$en" = 1 ] && [ "${cq:-1}" -eq 0 ] 2>/dev/null && [ "${ci:-1}" = 0 ]; then
        printf 'skip\n'
    else
        printf 'gate\n'
    fi
}

# basefail_fix_decision <external_ref> <name> <gate_out> — returns 0 (a green base-fix)
# when external_ref names this repository's base-fix suite (basefail:<name>:<suite>) AND
# the branch's own section of gate_out shows that suite green (absent from its RED/TIMEOUT/
# FAILED list) rather than the same red the base has. Returns 1 for every other branch,
# including one whose external_ref names a *different* repository's base-fix — a bead's
# fix for repo A must never certify a held branch in repo B just because both gate runs
# happened to fail.
basefail_fix_decision() {
    local _extref="$1" _name="$2" _gate_out="$3" _fse _br_had
    case "$_extref" in basefail:"$_name":*) : ;; *) return 1 ;; esac
    _fse="${_extref#basefail:$_name:}"
    [ -n "$_fse" ] && [ "$_fse" != "-" ] || return 1
    _br_had="$(printf '%s' "$_gate_out" | awk -v s="$_fse" '
        /^--- this branch/{p=1;next}
        p&&/^(---|gate:)/{p=0}
        p{for(i=1;i<NF;i++) if($i==s&&($(i+1)~/^(RED|TIMEOUT|FAILED)$/||($(i+1)=="was"&&$(i+2)=="killed"))){print "yes";exit}}
    ')"
    [ -z "$_br_had" ]
}
