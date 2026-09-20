#!/usr/bin/env bash
#
# test-delivers-parity.sh — aeon.sh and sentinel.sh must give the same verdict for every
#   delivers type; a single case block shared from lib.sh is the remedy. Until then, this
#   test is the guard: it runs the extracted case block from each file for each type and
#   asserts agreement, so adding a type to one without the other shows red here immediately.
#
# WHAT IS TESTED. For each type that does not require an external call (action, check, and
# an unrecognised type), the case block is extracted from each file and evaled. beads and
# note/report require bdjson and file-system state; those are covered by their own suites
# (test-aeon-verdict.sh, test-check5-delivers-action.sh).
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control). Before comparing verdicts across
# files, the extraction is verified: a type that both files are known to reject (check with
# no command) must produce ok=0 from both before the cross-file comparison is trusted.
#
# defect: sp-vkozc
# covers: spira/aeon.sh spira/sentinel.sh
# hermetic-ok: no testdb, no git, no external processes beyond shell
# timeout: 30
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
eq()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "files disagree: aeon=[$2] sentinel=[$3]"; }

AEON="$HERE/aeon.sh"
SENTINEL="$HERE/sentinel.sh"

# Extract the delivers-type case block. There is exactly one 'case "$_dtype" in' in each
# file; awk stops at the first esac that closes it.
case_block() { awk '/case "\$_dtype" in/{p=1} p{print} p && /^[[:space:]]*esac/{p=0;exit}' "$1"; }

# Run the extracted case block for one type. Sets _delivers_ok and _delivers_fail.
# Variables the beads/note/report branches reference (bdjson, BEAD_ID, id, SESSION_EPOCH,
# started_at) are defined to safe dummies so an accidental branch-fall does not error.
verdict() {  # verdict <file> <dtype> <dval> — prints "ok|fail_message"
    local _f="$1" _dtype="$2" _dval="$3"
    local _cb; _cb="$(case_block "$_f")" || { printf 'ERR|could not extract case block from %s' "$_f"; return; }
    (
        _delivers_ok=1 _delivers_fail=""
        BEAD_ID=t id=t SESSION_EPOCH=0 started_at=""
        bdjson() { printf '[]'; }
        eval "$_cb" 2>/dev/null
        printf '%s|%s' "$_delivers_ok" "$_delivers_fail"
    )
}

echo "test-delivers-parity.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL — extraction is working (check without command rejects in both files):"
# ======================================================================================
a_out="$(verdict "$AEON"     check check)"
s_out="$(verdict "$SENTINEL" check check)"
is "aeon.sh:    check-no-cmd ok=0"       0     "${a_out%%|*}"
is "sentinel.sh: check-no-cmd ok=0"      0     "${s_out%%|*}"
want "aeon.sh:    check message"    "has no command" "${a_out#*|}"
want "sentinel.sh: check message"   "has no command" "${s_out#*|}"

# ======================================================================================
echo
echo "action — both files must accept (ok=1):"
# ======================================================================================
a_out="$(verdict "$AEON"     action action)"
s_out="$(verdict "$SENTINEL" action action)"
is "aeon.sh:    action ok=1"       1 "${a_out%%|*}"
is "sentinel.sh: action ok=1"      1 "${s_out%%|*}"
eq "action verdict agrees"         "${a_out%%|*}" "${s_out%%|*}"

# ======================================================================================
echo
echo "check:true — both files must accept (ok=1):"
# ======================================================================================
a_out="$(verdict "$AEON"     check true)"
s_out="$(verdict "$SENTINEL" check true)"
is "aeon.sh:    check:true ok=1"    1 "${a_out%%|*}"
is "sentinel.sh: check:true ok=1"   1 "${s_out%%|*}"
eq "check:true verdict agrees"      "${a_out%%|*}" "${s_out%%|*}"

# ======================================================================================
echo
echo "check:false — both files must reject (ok=0):"
# ======================================================================================
a_out="$(verdict "$AEON"     check false)"
s_out="$(verdict "$SENTINEL" check false)"
is "aeon.sh:    check:false ok=0"   0 "${a_out%%|*}"
is "sentinel.sh: check:false ok=0"  0 "${s_out%%|*}"
eq "check:false verdict agrees"     "${a_out%%|*}" "${s_out%%|*}"

# ======================================================================================
echo
echo "unrecognised type — both files must reject with the same type list:"
# ======================================================================================
a_out="$(verdict "$AEON"     bogustype bogustype)"
s_out="$(verdict "$SENTINEL" bogustype bogustype)"
is "aeon.sh:    bogustype ok=0"    0 "${a_out%%|*}"
is "sentinel.sh: bogustype ok=0"   0 "${s_out%%|*}"
eq "bogustype fail message agrees" "${a_out#*|}" "${s_out#*|}"

# ======================================================================================
echo
echo "structural: action) branch appears in both files:"
# ======================================================================================
a_cnt="$(grep -c 'action)' "$AEON"     2>/dev/null || echo 0)"
s_cnt="$(grep -c 'action)' "$SENTINEL" 2>/dev/null || echo 0)"
[ "${a_cnt:-0}" -ge 1 ] && ok "aeon.sh has action) branch"     || bad "aeon.sh has action) branch"     "count=$a_cnt"
[ "${s_cnt:-0}" -ge 1 ] && ok "sentinel.sh has action) branch" || bad "sentinel.sh has action) branch" "count=$s_cnt"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
