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

# prior_pass_suites <gate-run.sh --status output> -> the suite list a recorded PASS
# covered, or empty when the text carries no such record — gate-run.sh --status exited
# non-zero, or nothing has ever gated this (tip, base) pair. Reading it back here is what
# lets a cert-gate-red reopen name whether the failing suite was ever in the aeon's own
# gate's scope, rather than leave that the one fact nothing recorded (sp-0pk2x).
prior_pass_suites() {
    printf '%s\n' "$1" | sed -n 's/^gate-run: gate PASS covered suites: //p' | tail -1
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
