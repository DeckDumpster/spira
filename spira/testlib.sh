#!/usr/bin/env bash
# testlib.sh — the one assertion library every spira/test-*.sh suite sources.
#
# Sourced, never executed:
#   HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   . "$HERE/testlib.sh"
#
# WHY THIS EXISTS. 459 of 466 suites define their own ok()/want()/nowant(), so a red
# prints one of three-plus wordings, 25 suites print no summary at all, and some skips
# exit 0 and are counted as green passes. Nothing downstream — gate-diag.sh, suites.sh,
# forge.sh's attribution — can read a suite's result without re-parsing prose it was never
# given a contract for. This is that contract: fixed function names, TAP 14 on stdout,
# and one JSONL row per case when a batch runner asks for it.
#
# FUNCTIONS, NAMED EXACTLY AS THE DE FACTO HELPERS THEY REPLACE (mechanical migration is
# the point — sp-qvjzb renames call sites, not call shapes):
#   ok    <name>                     record a pass
#   bad   <name> [detail]            record a failure
#   is      <name> <expected> <actual>   pass iff the two strings are equal
#   want    <name> <needle> <haystack>   pass iff haystack contains needle
#   nowant  <name> <needle> <haystack>   pass iff haystack does not contain needle
#   wantrc  <name> <expected-rc> <actual-rc>  pass iff the two codes are equal
#   plan  <n>                        declare the case count up front (optional — a suite
#                                     that never calls it gets a trailing plan instead)
#   skip  <reason>                   the WHOLE SUITE cannot run here: TAP skip-all,
#                                     exit 77 (the automake-skip code testenv-batch.sh and
#                                     suites.sh already special-case). A skip is never a
#                                     pass: it is its own status in TAP, in the JSONL, and
#                                     in every counter this library keeps.
#   bail  <reason>                   something is broken, not merely inapplicable: TAP
#                                     "Bail out!", exit 2, immediately. Never retried as
#                                     though it were a normal red.
#   tl_summary                       print the totals and end the suite; its exit status
#                                     is the suite's exit status, so it MUST be the last
#                                     statement in the file (not called from a subshell,
#                                     not followed by anything that resets $?).
#
# CASE IDS. The <name> argument IS the case id — the same string appears as the TAP
# description and as the JSONL "case" field. Suites already give each assertion a
# stable, unique, human-readable name; a second identifier alongside it would be a second
# thing to keep in sync with nothing enforcing that they agree.
#
# HEADER CONVENTIONS, read by the runner (suite-covers.sh), not by this file at suite
# run time:
#   # tier: T0..T4          how expensive a suite is, declared by its author
#   # covers: <space-separated tokens>   a path glob (existing convention) or a UC id
#                           of the form UC-<area>-NN (the catalogue sp-qu948 defines);
#                           the two kinds of token share one line and are told apart by
#                           the "UC-" prefix, so a suite that covers both code and a use
#                           case declares them together rather than in two directives that
#                           could drift apart.
#
# JSONL. Silent unless SPIRA_TESTLIB_JSONL names a writable file — a suite run by hand
# has no batch collecting rows and should not block on a path nobody gave it. Set by the
# batch runner, one shared file per batch: every case from every suite in that batch
# appends here, so the file is opened for append and never truncated by this library.
#
# NO PIPE INTO A MATCHER THAT EXITS EARLY IN want/nowant. Bash's [[ ]] is used directly
# rather than grep, so law-no-grep-q-under-pipefail does not apply here, but the same
# instinct holds: a helper that can fail for a reason OTHER than the assertion itself
# (a missing binary, a broken pipe) must not be indistinguishable from the assertion
# failing. [[ ]] has no such failure mode.
set -u

_TL_NUM=0
_TL_PASS=0
_TL_FAIL=0
_TL_SKIP=0
_TL_PLANNED=""
_TL_INITED=0
_TL_LAST_S=$SECONDS
_TL_SUITE="$(basename "${BASH_SOURCE[1]:-${0:-suite}}")"
_TL_JSONL="${SPIRA_TESTLIB_JSONL:-}"
_TL_SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$_TL_SELF/suite-covers.sh"
_TL_TIER="$(suite_tier_of "${BASH_SOURCE[1]:-$0}")"
_TL_UC="$(suite_uc_of "${BASH_SOURCE[1]:-$0}")"
unset _TL_SELF

_tl_init() {
    [ "$_TL_INITED" = 1 ] && return 0
    _TL_INITED=1
    printf 'TAP version 14\n'
}

# _tl_json_escape <string> -> the string with \, " and control chars made JSON-safe.
# No external process: this runs once per assertion and a suite can have hundreds.
_tl_json_escape() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    s="${s//$'\n'/\\n}"
    s="${s//$'\t'/\\t}"
    printf '%s' "$s"
}

# _tl_jsonl <case> <status> [detail] -> append one row, or do nothing when no sink is set.
_tl_jsonl() {
    [ -n "$_TL_JSONL" ] || return 0
    local case_id="$1" status="$2" detail="${3:-}" secs=$(( SECONDS - _TL_LAST_S ))
    _TL_LAST_S=$SECONDS
    local uc_json="[]"
    if [ -n "$_TL_UC" ]; then
        uc_json="$(printf '%s' "$_TL_UC" | awk '{
            printf "["
            for (i=1;i<=NF;i++) { printf "%s\"%s\"", (i>1?",":""), $i }
            printf "]"
        }')"
    fi
    printf '{"suite":"%s","tier":"%s","case":"%s","status":"%s","seconds":%s,"uc":%s,"detail":"%s"}\n' \
        "$(_tl_json_escape "$_TL_SUITE")" "$(_tl_json_escape "$_TL_TIER")" \
        "$(_tl_json_escape "$case_id")" "$status" "$secs" "$uc_json" \
        "$(_tl_json_escape "$detail")" >> "$_TL_JSONL"
}

ok() {
    _tl_init
    _TL_NUM=$((_TL_NUM + 1)); _TL_PASS=$((_TL_PASS + 1))
    printf 'ok %s - %s\n' "$_TL_NUM" "$1"
    _tl_jsonl "$1" pass
}

bad() {
    _tl_init
    _TL_NUM=$((_TL_NUM + 1)); _TL_FAIL=$((_TL_FAIL + 1))
    printf 'not ok %s - %s\n' "$_TL_NUM" "$1"
    [ -n "${2:-}" ] && printf '# %s\n' "$2"
    _tl_jsonl "$1" fail "${2:-}"
}

is() {      # is <name> <expected> <actual>
    [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"
}

want() {    # want <name> <needle> <haystack>
    [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"
}

nowant() {  # nowant <name> <needle> <haystack>
    [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"
}

wantrc() {  # wantrc <name> <expected-rc> <actual-rc>
    [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted rc=$2 got rc=$3"
}

plan() {    # plan <n> — must be called before the first ok/bad/want/nowant/wantrc
    _tl_init
    _TL_PLANNED="$1"
    printf '1..%s\n' "$_TL_PLANNED"
}

# skip <reason> — the whole suite cannot run here. TAP's skip-all form, and exit 77:
# the automake-skip code the batch runner and the timed runner both already treat as
# distinct from red. Never call this after any case has already run — a partial suite
# that then claims "skipped" would hide the cases that did run.
skip() {
    _tl_init
    if [ "$_TL_NUM" -gt 0 ]; then
        # A partial suite claiming "skipped" would hide the cases that already ran —
        # that is a suite-authoring bug, not a real skip, so it fails loudly rather
        # than being folded into either outcome.
        bail "skip called after $_TL_NUM case(s) already ran: $1"
    fi
    _TL_SKIP=$((_TL_SKIP + 1))
    printf '1..0 # SKIP %s\n' "$1"
    _tl_jsonl "(suite)" skip "$1"
    exit 77
}

# bail <reason> — something is broken, not merely inapplicable. Stops the run immediately;
# a bailed suite must never be retried as though its failure were a normal assertion red.
bail() {
    _tl_init
    printf 'Bail out! %s\n' "$1"
    _tl_jsonl "(suite)" bail "$1"
    exit 2
}

# tl_summary — must be the last statement in the suite; its exit status becomes the
# suite's exit status. Prints the trailing plan when `plan` was never called (TAP permits
# the plan at either end), then the human-readable totals every reader of this tree
# already expects, then returns pass/fail as $fail -eq 0 always has.
tl_summary() {
    _tl_init
    [ -n "$_TL_PLANNED" ] || printf '1..%s\n' "$_TL_NUM"
    printf '\n%d passed, %d failed, %d skipped\n' "$_TL_PASS" "$_TL_FAIL" "$_TL_SKIP"
    # suites.sh's setup-fault detector greps this exact line (`ASSERTIONS 0`) to tell a
    # suite that failed before its first case from one whose cases actually ran and lost.
    printf 'ASSERTIONS %d\n' "$((_TL_PASS + _TL_FAIL))"
    [ "$_TL_FAIL" -eq 0 ]
}

# TAP version 14 MUST be the first line of output, so it is printed at SOURCE time
# rather than lazily on first use: a suite that prints its own name (the common
# convention this library inherits) does so after `. testlib.sh`, never before.
_tl_init
