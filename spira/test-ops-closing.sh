#!/usr/bin/env bash
#
# test-ops-closing.sh — `strand close-reason`, the phrase table the close-reason fence and
# detect_invalid_closed share, and the ops.md instruction check.
#
# The fence wiring (a submission whose reason admits unfinished work goes back to REWORK, a
# clean one stands SUBMITTED) and the retired RUNE_SOP_REQUIRED warning are dwarf tests
# (`cargo test -p dwarf close_reason retired_sop`).
#
# defect: sp-9pyr
# tier: T1
# covers: dwarf/src/* sop/src/*.rs strand/src/closereason.rs spira/runes/ops.rune spira/runes/ops.md spira/test-ops-closing.sh
# covers: dwarf/src/* strand/src/closereason.rs spira/runes/ops.rune spira/test-ops-closing.sh UC-dwarf-execution-16
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90

echo "test-ops-closing.sh"


echo
echo "T1: strand close-reason, unit directly — no dwarf run, no bd"
# ===========================================================================================
# THE FENCE AND detect_invalid_closed SHARE strand::closereason so they cannot disagree
# about which phrase triggers a refusal. Testing it directly, rather than only through a
# full dwarf run per case, is what "already standalone" is for.
crf() {   # crf <reason> -> sets CRF_RC and CRF_OUT (the matched phrase, if any)
    CRF_OUT="$(strand close-reason "$1" 2>/dev/null)"; CRF_RC=$?
}

crf "done"
wantrc "a clean reason: no match" 1 "$CRF_RC"

crf "TEMPORARY WORKAROUND: x is set until y lands"
wantrc "a plain admit matches" 0 "$CRF_RC"
is     "and names the matched phrase" "TEMPORARY WORKAROUND" "$CRF_OUT"

crf 'pair added — bad-reason ("TEMPORARY WORKAROUND") is reopened; clean reason stays closed'
wantrc "a double-quoted mention is masked, not an admission" 1 "$CRF_RC"

crf "the thing (a TEMPORARY WORKAROUND) was avoided entirely"
wantrc "a parenthetical mention is masked too" 1 "$CRF_RC"

crf 'the `TEMPORARY WORKAROUND` phrase, avoided'
wantrc "a backtick-quoted mention is masked too" 1 "$CRF_RC"

crf "see close-reason-flags for the TEMPORARY WORKAROUND check"
wantrc "a line naming this file is a mention, not an admission" 1 "$CRF_RC"

crf "PERMANENT FIX NEEDED here"
wantrc "PERMANENT FIX NEEDED matches" 0 "$CRF_RC"

crf "mitigated-only for now"
wantrc "mitigated-only matches" 0 "$CRF_RC"

crf "TODO: fix this properly"
wantrc "a bare TODO matches" 0 "$CRF_RC"

crf "temporarily fixed the issue for the release"
wantrc "temporarily fixed matches" 0 "$CRF_RC"

crf "a TEMPORARY workaround was used, in lowercase"
wantrc "matching is case-insensitive" 0 "$CRF_RC"

OPS_MD="$HERE/runes/ops.md"
if grep -Eq 'commit that page|SOP page is normally' "$OPS_MD"; then _rc=1; else _rc=0; fi
wantrc "ops.md does not tell the dwarf to commit a page the broker wrote" 0 "$_rc"
grep -q "not in your worktree" "$OPS_MD"; wantrc "ops.md says where sop write puts the page" 0 "$?"
_probe="commit that page"; printf '%s\n' "$_probe" | grep -Eq 'commit that page|SOP page is normally'
wantrc "the matcher fires on the old instruction" 0 "$?"


tl_summary
