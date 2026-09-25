#!/usr/bin/env bash
#
# test-aeon-exit.sh — aeon.sh exits 0 after a successful bead close even when the
#                     claude session itself exits 1; a genuinely failed sweep still exits 1.
#
#   ./test-aeon-exit.sh
#
# THE DEFECT THIS TESTS. ops and qa run as named systemd units (spira-ops.service,
# spira-qa.service). Unlike transient units (systemd-run --collect, which are torn down
# on any exit), named units enter the FAILED state when their ExecStart exits non-zero.
# The aeon that closed sp-637b exited 1 — a stray non-zero from the last command the
# verdict block happened to run (a `bdq note` that returned non-zero) — and the unit
# was stuck in FAILED, making every `systemctl --state=failed` show a false alarm. An
# alert that is always firing is one nobody reads (law-alerts-must-be-actionable).
#
# WHAT IS TESTED:
#   1. Bead mode: when the bead is closed, aeon exits 0 even when claude exits 1.
#      Positive control: when the bead is NOT closed, aeon exits non-zero.
#   2. Sweep mode: when claude ran (produced tool calls and a result), aeon exits 0
#      even when claude exits 1.
#      Positive control: when the session was refused (no tool calls, no result),
#      aeon exits non-zero — a real ops failure remains visible.
#
# THE CLAUDE EXIT CODE IS THE VARIABLE UNDER TEST. The shim is configured to exit 0 or 1
# to test each case independently from what the session did. The shim also controls whether
# it closes the bead and what output it writes, which together drive the exit code logic.
#
# Driven through the REAL aeon.sh against a real bd on a throwaway fixture, with a shim
# standing in for the model (law-prefer-the-real-dependency).
#
# defect: sp-iu10
# covers: spira/aeon.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-exit
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonexit || { echo "test-aeon-exit: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

# THE SHIM WRITES $TMP/shim-act TO CONTROL BEHAVIOUR. The claude guard ensures this suite
# does not silently run the real model when conf.sh replaces $PATH.
BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-exit: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }

# The shim is driven by two files:
#   $TMP/shim-act:   what the session does (commit+close, commit-only, refused)
#   $TMP/shim-rc:    what exit code the shim uses (0 or 1)
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
# One real tool call so the session looks like it acted — session_outcome reads tool calls.
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' /dev/stdin 2>/dev/null | head -1)"
# Read the bead id from stdin which was already consumed — use the env instead.
cat /dev/stdin > /dev/null 2>&1
id="$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
act="$(cat "$TMP/shim-act" 2>/dev/null)"
case "$act" in
    commit-close)
        printf 'my work\n' >> f
        git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
        BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
        printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
        ;;
    commit-only)
        printf 'my work\n' >> f
        git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
        printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
        ;;
    refused)
        # Simulate an API refusal: no tool calls visible in the result, exit 1.
        printf '{"type":"result","subtype":"error","is_error":true,"result":"you have reached your session limit","duration_ms":100,"num_turns":0,"total_cost_usd":0}\n'
        ;;
esac
exit "$(cat "$TMP/shim-rc" 2>/dev/null || echo 0)"
SHIM
chmod +x "$BIN/claude"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}
run_aeon() {
    rm -rf "$SPIRA_RUN/worktree"
    "$HERE/aeon.sh" builder > "$TMP/out" 2>&1
    echo $?
}
field() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1],""))' "$2" 2>/dev/null
}
fresh() { testdb_reset; }

echo "test-aeon-exit.sh"

# ======================================================================================
echo
echo "bead closed, claude exits 0 — aeon exits 0 (baseline):"
# ======================================================================================
fresh; seed sp-ex-1
printf commit-close > "$TMP/shim-act"; printf 0 > "$TMP/shim-rc"
rc="$(run_aeon)"
is "bead is converted to submitted, not left closed" "open" "$(field sp-ex-1 status)"
is "aeon exits 0"        "0"      "$rc"

# ======================================================================================
echo
echo "bead closed, claude exits 1 — aeon still exits 0 (the fix):"
# ======================================================================================
# THE DEFECT THIS REPRODUCES. The claude CLI exits 1 when a tool call returned non-zero
# even though the session itself did the work and closed the bead. A named unit (ops, qa)
# must not enter FAILED state because of this stray exit code.
fresh; seed sp-ex-2
printf commit-close > "$TMP/shim-act"; printf 1 > "$TMP/shim-rc"
rc="$(run_aeon)"
is "bead is converted to submitted, not left closed" "open" "$(field sp-ex-2 status)"
is "aeon exits 0 despite claude rc=1" "0" "$rc"
want "ledger still records the real rc" "rc=1" "$(grep 'done builder sp-ex-2' "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null)"
want "and records the submitted status" "status=submitted" "$(grep 'done builder sp-ex-2' "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null)"

# ======================================================================================
echo
echo "POSITIVE CONTROL — bead NOT closed, claude exits 1 — aeon exits non-zero:"
# ======================================================================================
# If this assertion fails, the fix above is too broad and would hide real failures.
fresh; seed sp-ex-3
printf commit-only > "$TMP/shim-act"; printf 1 > "$TMP/shim-rc"
rc="$(run_aeon)"
is "bead is open (never closed)"  "open"  "$(field sp-ex-3 status)"
is "aeon exits non-zero"          "1"     "$rc"

# ======================================================================================
echo
echo "sweep: claude exits 1 but ran (unlanded) — aeon exits 0 (ops/qa sweep fix):"
# ======================================================================================
# In sweep mode (aeon.sh <fayth> --sweep), the service must not enter FAILED just because
# the claude CLI had a non-zero exit from a stray tool-call error.
cat > "$SPIRA_HOME/chamber/sweeper.fayth" <<SFAYTH
FAYTH_NAME=sweeper
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
SFAYTH
printf 'sweep {{BEAD_ID}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/sweeper.md"

# The sweep shim emits the same events but claude exits 1.
cat > "$BIN/claude" <<'SHIM2'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":2000,"num_turns":1,"total_cost_usd":0.001}\n'
exit "$(cat "$TMP/shim-rc" 2>/dev/null || echo 0)"
SHIM2
chmod +x "$BIN/claude"

printf 1 > "$TMP/shim-rc"
sweep_rc="$("$HERE/aeon.sh" sweeper --sweep --prompt "check pipeline" > "$TMP/sweep-out" 2>&1; echo $?)"
is "sweep with claude rc=1 exits 0" "0" "$sweep_rc"

# ======================================================================================
echo
echo "POSITIVE CONTROL — sweep refused (no tool calls) with claude rc=1 — aeon exits non-zero:"
# ======================================================================================
# A session that was refused by the API emitted no tool calls. The named service SHOULD
# enter FAILED in this case — that is a real failure worth seeing.
cat > "$BIN/claude" <<'SHIM3'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
# No tool calls — simulates an API refusal where the session was rejected before acting.
printf '{"type":"result","subtype":"error","is_error":true,"result":"you have reached your session limit","duration_ms":100,"num_turns":0,"total_cost_usd":0}\n'
exit "$(cat "$TMP/shim-rc" 2>/dev/null || echo 1)"
SHIM3
chmod +x "$BIN/claude"

printf 1 > "$TMP/shim-rc"
refused_rc="$("$HERE/aeon.sh" sweeper --sweep --prompt "check pipeline" > "$TMP/refused-out" 2>&1; echo $?)"
is "refused sweep exits non-zero" "1" "$refused_rc"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
