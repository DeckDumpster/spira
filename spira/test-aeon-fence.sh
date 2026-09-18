#!/usr/bin/env bash
# covers: spira/hooks/aeon-fence.sh spira/aeon.sh spira/suites.sh spira/chamber/archivist.md
# defect: sp-kz8ob
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
refuse() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "must not contain [$2] in [$3]"; }

echo "test-aeon-fence.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

FENCE="$HERE/hooks/aeon-fence.sh"
FAKE_RUN="$TMP/run"
FAKE_PROD="$TMP/prod"
mkdir -p "$FAKE_RUN" "$FAKE_PROD"

# fence_run <cmd> [VAR=val ...]: pipe a Bash-tool JSON payload to the fence.
# Uses python3 json.dumps so the payload is valid for any command, including
# those with newlines (heredocs) or embedded quotes.
# Extra positional args are env-var assignments passed to env(1).
fence_run() {
    local cmd="$1"; shift
    local payload
    payload="$(printf '%s' "$cmd" | python3 -c 'import json, sys; cmd = sys.stdin.read(); print(json.dumps({"tool_name":"Bash","tool_input":{"command":cmd}}))')"
    printf '%s' "$payload" | env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$FAKE_RUN" SPIRA_PROD="$FAKE_PROD" "$@" \
        bash "$FENCE" 2>/dev/null
}

# ===========================================================================
echo
echo "POSITIVE CONTROL — fence detects batch.sh in aeon session before asserting B:"
# ===========================================================================
pc="$(fence_run "bash spira/batch.sh spira" SPIRA_AEON=test-aeon)"
want "POSITIVE: fence emits decision:block for batch.sh" '"decision":"block"' "$pc"

# ===========================================================================
echo
echo "A — aeon session: queue-operating commands are blocked:"
# ===========================================================================

out="$(fence_run "bash spira/batch.sh spira" SPIRA_AEON=test-aeon)"
want "A1: batch.sh → decision:block"    '"decision":"block"' "$out"
want "A1: reason mentions sp-kz8ob"     "sp-kz8ob"          "$out"

out="$(fence_run "git push origin main" SPIRA_AEON=test-aeon)"
want "A2: git push → decision:block"    '"decision":"block"' "$out"

out="$(fence_run "gh pr create --title x" SPIRA_AEON=test-aeon)"
want "A3: gh pr create → decision:block" '"decision":"block"' "$out"

out="$(fence_run "bash spira/queue.sh flush spira" SPIRA_AEON=test-aeon)"
want "A4: queue.sh flush → decision:block" '"decision":"block"' "$out"

# queue.sh stats is the only read-only subcommand — must not be blocked.
out="$(fence_run "bash spira/queue.sh stats" SPIRA_AEON=test-aeon)"
refuse "A5: queue.sh stats NOT blocked" '"decision":"block"' "$out"

out="$(fence_run "printf x > ${FAKE_RUN}/landstate/x" SPIRA_AEON=test-aeon)"
want "A6: write to SPIRA_RUN/landstate → decision:block" '"decision":"block"' "$out"

out="$(fence_run "rm -rf ${FAKE_PROD}/releases" SPIRA_AEON=test-aeon)"
want "A7: write to SPIRA_PROD → decision:block" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "B — non-aeon session: same commands pass through:"
# ===========================================================================

out="$(fence_run "bash spira/batch.sh spira")"
refuse "B1: batch.sh not blocked without SPIRA_AEON" '"decision":"block"' "$out"

out="$(fence_run "git push origin main")"
refuse "B2: git push not blocked without SPIRA_AEON" '"decision":"block"' "$out"

out="$(fence_run "bash spira/batch.sh spira" SPIRA_AEON=test-aeon SPIRA_AEON_OVERRIDE=1)"
refuse "B3: SPIRA_AEON_OVERRIDE=1 bypasses fence"   '"decision":"block"' "$out"

# ===========================================================================
echo
echo "C — suites.sh quarantine refuses from an aeon session:"
# ===========================================================================
# POSITIVE CONTROL: without SPIRA_AEON the guard does not fire (suites.sh may still
# fail for other reasons, but the aeon-guard message must be absent).
out_c0="$(SPIRA_AEON="" SPIRA_CONF=/nonexistent SPIRA_RUN="$FAKE_RUN" \
    bash "$HERE/suites.sh" quarantine nonexistent-suite.sh bead-id "reason" 2>&1 || true)"
refuse "C0 POSITIVE: guard silent without SPIRA_AEON" "aeons may not" "$out_c0"

# Guard must fire and return non-zero.
out_c1="$(SPIRA_AEON=test-aeon SPIRA_CONF=/nonexistent SPIRA_RUN="$FAKE_RUN" \
    bash "$HERE/suites.sh" quarantine nonexistent-suite.sh bead-id "reason" 2>&1)"
rc_c1=$?
is   "C1: quarantine exits non-zero in aeon session"    "1" "$rc_c1"
want "C1: refusal mentions 'aeons may not'"             "aeons may not" "$out_c1"
want "C1: refusal mentions operator or Ops session"     "operator" "$out_c1"

# ===========================================================================
echo
echo "E — test runner: suites whose names contain a blocked script name pass through:"
# ===========================================================================
# POSITIVE CONTROL: the direct call is still blocked (pattern requires preceding /).
out="$(fence_run "bash spira/verdict.sh push spira" SPIRA_AEON=test-aeon)"
want "E0 POSITIVE: spira/verdict.sh directly still blocked" '"decision":"block"' "$out"

out="$(fence_run "bash spira/testenv-batch.sh --suites test-verdict.sh spira/sp-x" SPIRA_AEON=test-aeon)"
refuse "E1: testenv-batch with test-verdict.sh not blocked" '"decision":"block"' "$out"

out="$(fence_run "bash spira/testenv-batch.sh --suites test-batch.sh spira/sp-x" SPIRA_AEON=test-aeon)"
refuse "E2: testenv-batch with test-batch.sh not blocked"   '"decision":"block"' "$out"

out="$(fence_run "bash spira/testenv-batch.sh spira/sp-x" SPIRA_AEON=test-aeon)"
refuse "E3: testenv-batch.sh itself not blocked"            '"decision":"block"' "$out"

# ===========================================================================
echo
echo "F — prod-path and fenced-name appearing as prose (data) do not block:"
# ===========================================================================
# POSITIVE CONTROL: actual writes and direct invocations are still blocked.
out="$(fence_run "rm -rf ${FAKE_PROD}/releases" SPIRA_AEON=test-aeon)"
want "F0 POSITIVE: write to SPIRA_PROD still blocked" '"decision":"block"' "$out"

out="$(fence_run "bash spira/verdict.sh push spira" SPIRA_AEON=test-aeon)"
want "F1 POSITIVE: direct forge-script still blocked" '"decision":"block"' "$out"

# census.sh is a read-only events query; aeons may invoke it from SPIRA_PROD.
out="$(fence_run "bash \"${FAKE_PROD}/census.sh\" --with-suppressed" SPIRA_AEON=test-aeon)"
refuse "F2: census.sh from SPIRA_PROD NOT blocked" '"decision":"block"' "$out"

# A bead create passes description text via heredoc.  The prod path and a
# fenced script name in that body are data, not commands to execute.
_bd_cmd="$(printf 'bd -C /db create title --description - <<\047DESC\047\nbash "%s/census.sh" and /verdict.sh are mentioned\nDESC' "${FAKE_PROD}")"
out="$(fence_run "$_bd_cmd" SPIRA_AEON=test-aeon)"
refuse "F3: bd create with prod-path and fenced-name in heredoc NOT blocked" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "D — archivist.md prohibits filing production-operation beads:"
# ===========================================================================
ARCHIVIST="$HERE/chamber/archivist.md"

# POSITIVE CONTROL: prove the file is readable by checking a known section header.
archivist_text="$(cat "$ARCHIVIST")"
want "D0 POSITIVE: archivist.md has 'What you must not do'" \
    "What you must not do" "$archivist_text"

want "D1: prohibition names 'operating production'" "operating production" "$archivist_text"
want "D2: prohibition names 'Land PR'"              "Land PR"              "$archivist_text"
want "D3: prohibition names 'flush the queue'"      "flush the queue"      "$archivist_text"

# ===========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
