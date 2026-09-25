#!/usr/bin/env bash
# Sourced, never executed.
# gate-lib.sh — the pure functions gate.sh's verdict depends on, split out so a T1 suite
# can call them directly instead of paying for a full gate trial (git worktree, a branch
# command, on a red a base command too) to exercise one branch of one function.
#
# NOTHING HERE READS ARGV, $0 OR THE FILESYSTEM'S TREE STATE on its own account. Each
# function takes what it needs as an argument — a path to a file already written, a
# pre-computed hash, a branch name — so sourcing this file has no side effect and calling
# a function needs no git repository, no lock, no worktree. gate.sh supplies the git- and
# filesystem-facing glue (git rev-parse, cat "$0", flock) and passes the results in.
#
# Sourced, never executed:
#   . "$(dirname "$0")/gate-lib.sh"
set -u

# verdict <status> <reason> [message...] — the ONE way a gate run ends. See gate.sh for the
# full rationale; this is a plain relocation; its behaviour is unchanged by moving here.
verdict() {              # verdict <status> <reason> [message...]
    local st="$1" reason="$2"; shift 2
    local msg="$*"
    if [ "$st" != 0 ] && [ "$st" != "$SPIRA_GATE_NOVERDICT" ] && [ "$st" != "$SPIRA_GATE_BASEFAIL" ] \
       && [ -z "${msg//[[:space:]]/}" ]; then
        reason="no-evidence:$reason"; st="$SPIRA_GATE_NOVERDICT"
        msg="the gate returned a failure with no output at all — that is the machinery failing to run a check, not the branch failing one"
    fi
    [ -n "$msg" ] && printf '%s\n' "$msg" >&2
    printf 'gate: VERDICT=%s reason=%s branch=%s repo=%s suite=%s\n' \
        "$(spira_gate_outcome "$st")" "$reason" "$BR" "${REPO_NAME:-?}" "${GATE_SUITE:--}" >&2
    trap - EXIT
    rm -f "${FILELIST:-}" 2>/dev/null
    rm -f "${TREE:-}.lock.holder" 2>/dev/null || true
    if [ "${HELD_LOCK:-0}" = 1 ] && [ -n "${TREE:-}" ] && [ -e "${TREE}/.git" ] && [ "$st" != 0 ]; then
        git -C "${REPO:-/nonexistent}" worktree remove --force "$TREE" 2>/dev/null || true
    fi
    command -v gate_meter >/dev/null 2>&1 && gate_meter "$st" "$reason"
    command -v yield_note >/dev/null 2>&1 && yield_note "$st" "$reason"
    exit "$st"
}

# gate_tree_key <branch> -> a filesystem-safe suffix for the branch's own gate tree/lock.
gate_tree_key() {
    printf '%s' "$1" | tr '/' '-' | tr -c 'A-Za-z0-9.-' '-'
}

# gate_key_hash <repo> <tree> <files_h> <cmd_h> <harness_h> <suites> <bead> -> sha256.
#
# Pure string concatenation and hashing. gate.sh's own gate_key() does the git/file reads
# (rev-parse, cat "$0" "$EXCLUDE" "$SKEW", sha256sum of $files/$CMD) and passes the results
# here — so a T1 row can assert "changing one input moves the key" against fixed strings,
# without a git repository at all.
gate_key_hash() {
    printf '%s\n' "$1 $2 $3 $4 $5 suites=$6 bead=$7" | sha256sum | cut -d" " -f1
}

# cache_fresh <entry-file> <ttl> <now> -> 0 and "when|by" on stdout if the entry is fresh,
# 1 (nothing on stdout) if stale, unreadable, TTL is not a positive integer, or the entry
# carries no numeric `at=`.
#
# NO eval. The original inline check built an eval string from `sed` over the entry file's
# `when=`/`by=`/`at=` lines (gate.sh gap #10) — a `when=` containing `$(...)` would run it.
# This reads each line with `read`, which never interprets its value; a hostile `when=` is
# just a string that gets printed later, exactly as harmless as any other cached label.
cache_fresh() {
    local entry="$1" ttl="$2" now="$3" k v when="" by="" at="" age
    case "$ttl" in ''|*[!0-9]*) return 1 ;; esac
    [ -r "$entry" ] || return 1
    while IFS='=' read -r k v; do
        case "$k" in
            when) when="$v" ;;
            by)   by="$v" ;;
            at)   at="$v" ;;
        esac
    done < "$entry"
    case "$at" in ''|*[!0-9]*) return 1 ;; esac
    age=$(( now - at ))
    [ "$age" -ge 0 ] && [ "$age" -lt "$ttl" ] || return 1
    printf '%s|%s\n' "${when:-an earlier time}" "${by:-unknown caller}"
}

# red_suites <gate output> -> one suite per line, as the batch runner reports them.
red_suites() {
    printf '%s\n' "$1" | awk '{
        for (i = 1; i < NF; i++)
            if ($i ~ /\.sh$/ && ($(i+1) ~ /^(RED|TIMEOUT|FAILED)$/ || ($(i+1) == "was" && $(i+2) == "killed")))
                if (!seen[$i]++) print $i
    }'
}

# timed_out_suites <gate output> -> only the suites the watchdog killed, never a genuine
# FAIL — a killed suite proves nothing about whether it would have passed.
timed_out_suites() {
    printf '%s\n' "$1" | awk '{
        for (i = 1; i < NF; i++)
            if ($i ~ /\.sh$/ && ($(i+1) == "TIMEOUT" || ($(i+1) == "was" && $(i+2) == "killed")))
                if (!seen[$i]++) print $i
    }'
}

# gate_attribute <branch_rc> <branch_out_file> <base_ran> <base_rc> <base_out_file>
#   -> one line "<class> <suite>" on stdout, class in:
#        branch-red      the branch is at fault (base passed, or a suite red only on the
#                         branch even though the base is independently red elsewhere)
#        base-red        the base itself fails on a suite the branch also fails
#        base-timeout    the base's only reds were suites the watchdog killed — inconclusive
#        base-untestable the base trial itself could not be run (didn't reach here for 75/124)
#
# THIS IS THE WHOLE CLASSIFICATION RULE, as a pure function of two trials' captured output.
# Previously it was six-plus full gate runs (two git trials each) that could only ever prove
# their own one path through the logic. gate.sh keeps deciding nothing here: it hands this
# function the two outputs, switches on the class it returns, and uses its own copies of
# red_suites()/timed_out_suites() only to fill in the message text for whichever class won.
gate_attribute() {
    local branch_rc="$1" branch_out_file="$2" base_ran="$3" base_rc="$4" base_out_file="$5"
    local branch_out base_out base_reds branch_only base_timeouts base_genuine suite
    branch_out="$(cat "$branch_out_file" 2>/dev/null)"
    : "$branch_rc" # accepted for signature parity with the plan; not decision-relevant —
                   # this function is only reached after gate.sh has already observed the
                   # branch trial fail.

    if [ "$base_ran" != 1 ]; then
        printf 'base-untestable -\n'
        return 0
    fi

    if [ "$base_rc" = 0 ]; then
        suite="$(red_suites "$branch_out" | head -1)"
        printf 'branch-red %s\n' "${suite:--}"
        return 0
    fi

    base_out="$(cat "$base_out_file" 2>/dev/null)"
    base_reds="$(red_suites "$base_out")"
    branch_only="$(red_suites "$branch_out" | grep -vxF -f <(printf '%s\n' "$base_reds") || true)"
    if [ -n "$base_reds" ] && [ -n "$branch_only" ]; then
        printf 'branch-red %s\n' "$(printf '%s\n' "$branch_only" | head -1)"
        return 0
    fi

    base_timeouts="$(timed_out_suites "$base_out")"
    base_genuine="$(printf '%s\n' "$base_reds" | grep -vxF -f <(printf '%s\n' "$base_timeouts") || true)"
    if [ -n "$base_timeouts" ] && [ -z "$base_genuine" ]; then
        printf 'base-timeout %s\n' "$(printf '%s\n' "$base_timeouts" | head -1)"
        return 0
    fi

    suite="$(printf '%s\n' "$base_reds" | head -1)"
    printf 'base-red %s\n' "${suite:--}"
}
