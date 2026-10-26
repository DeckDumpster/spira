#!/usr/bin/env bash
#
# test-acceptance-waiver.sh — acceptance-waiver.sh picks waive/run per event and setting,
# and acceptance.yml's run step takes its waiver decision from it.
#
# tier: T1
# covers: spira/acceptance-waiver.sh .github/workflows/acceptance.yml
# host-reason: pure shell and text assertions; no container or database dependency
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SCRIPT="$HERE/acceptance-waiver.sh"
WF="$HERE/../.github/workflows/acceptance.yml"
. "$HERE/testlib.sh"

echo "test-acceptance-waiver.sh"

[ -x "$SCRIPT" ] || { bad "self-check: acceptance-waiver.sh exists and is executable" "missing"; tl_summary; exit 1; }

check() { # name expected args...
    local name="$1" want="$2"; shift 2
    local got; got="$(bash "$SCRIPT" "$@" 2>/dev/null)"
    if [ "$got" = "$want" ]; then ok "$name"; else bad "$name" "want '$want' got '$got'"; fi
}

check "tag push, standing set: waives (positive control)" waive --event push --standing standing
check "tag push, setting unset: runs upgrade phases"       run   --event push
check "tag push, other value: runs upgrade phases"         run   --event push --standing yes
check "dispatch, input true: waives"                       waive --event workflow_dispatch --input true
check "dispatch, input false, standing set: runs"          run   --event workflow_dispatch --input false --standing standing
check "dispatch, no input, setting unset: runs"            run   --event workflow_dispatch
_log="$(bash "$SCRIPT" --event push --standing standing 2>&1 >/dev/null)"
case "$_log" in *2026-10-23*ACCEPTANCE_WAIVE_UPGRADE*) ok "waiver log names the setting and the ruling" ;; *) bad "waiver log names the setting and the ruling" "$_log" ;; esac
bash "$SCRIPT" --event bogus >/dev/null 2>&1 && bad "unknown event refused" "exit 0" || ok "unknown event refused"

grep -q 'acceptance-waiver.sh --event "\$GITHUB_EVENT_NAME"' "$WF" \
    && ok "acceptance.yml takes the waiver decision from acceptance-waiver.sh" \
    || bad "acceptance.yml takes the waiver decision from acceptance-waiver.sh" "call not found"
grep -q 'vars.ACCEPTANCE_WAIVE_UPGRADE' "$WF" \
    && ok "acceptance.yml passes the standing setting" \
    || bad "acceptance.yml passes the standing setting" "variable not found"
grep -q 'one run per tag' "$WF" \
    && ok "acceptance.yml guard skips a duplicate run for the tag" \
    || bad "acceptance.yml guard skips a duplicate run for the tag" "guard not found"

tl_summary
