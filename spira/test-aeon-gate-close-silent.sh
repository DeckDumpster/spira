#!/usr/bin/env bash
#
# test-aeon-gate-close-silent.sh — a session that closes its bead while the gate has no
#                                   verdict (exit 3) or a FAIL verdict (exit 1) must leave
#                                   a note on the bead and a line in the aeon ledger.
#
# THE DEFECT THIS TESTS. gate_unfinished() returns 0 only when gate-run.sh --status exits 2
# (still running). At the closed-bead teardown path, exit codes 1 (FAIL) and 3 (no gate
# ran) fell through with no note and no ledger entry — the close looked identical to one
# that had obtained a passing verdict.
#
# WHAT IS TESTED:
#   1. POSITIVE CONTROL — gate status 2 (still running) writes a note, passes in both trees.
#   2. gate status 3 (no gate ran) writes a note (only passes after the fix).
#   3. gate status 1 (FAIL verdict) writes a note (only passes after the fix).
#
# covers: spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-gate-close-silent
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeongcls || { echo "test-aeon-gate-close-silent: could not build fixture database"; exit 1; }
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

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-gate-close-silent: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }

# gate-run.sh stub: exits with the code written to $TMP/gate-status-code
cat > "$SPIRA_HOME/gate-run.sh" <<'STUB'
#!/usr/bin/env bash
code="$(cat "${TMP}/gate-status-code" 2>/dev/null)"; code="${code:-2}"
case "$code" in
    2) echo "gate-run: still running for fixture — 10s so far, pid $$" ;;
    1) echo "gate-run: FAILED fixture in fixture after 10s" ;;
esac
exit "$code"
STUB
chmod +x "$SPIRA_HOME/gate-run.sh"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}

# The shim commits work and closes the bead, simulating a session that did the work.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf 'work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$BEAD_ID — done" >/dev/null 2>&1
BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" close "$BEAD_ID" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

run_aeon() {
    local code="$1"
    rm -rf "$SPIRA_RUN/worktree"
    printf '%s\n' "$code" > "$TMP/gate-status-code"
    "$HERE/aeon.sh" builder > "$TMP/out" 2>&1
}

bead_notes() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("notes", "") or "")' 2>/dev/null
}

fresh() { testdb_reset; }

echo "test-aeon-gate-close-silent.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL — st=2 (gate still running) writes a note (must pass in both trees):"
# ======================================================================================
fresh; seed sp-gcs-1
run_aeon 2
want "st=2: note mentions 'still running'" "still running" "$(bead_notes sp-gcs-1)"
want "st=2: log mentions gate still running" "closed with its gate still running" \
    "$(cat "$TMP/out" 2>/dev/null)"

# ======================================================================================
echo
echo "st=3 (no gate ever ran) — note and log entry must be written:"
# ======================================================================================
fresh; seed sp-gcs-2
run_aeon 3
want "st=3: note mentions 'no gate ran'" "no gate ran" "$(bead_notes sp-gcs-2)"
want "st=3: log mentions no gate verdict" "no gate verdict (none ran)" \
    "$(cat "$TMP/out" 2>/dev/null)"

# ======================================================================================
echo
echo "st=1 (FAIL verdict) — note and log entry must be written:"
# ======================================================================================
fresh; seed sp-gcs-3
run_aeon 1
want "st=1: note mentions 'FAIL verdict'" "FAIL verdict" "$(bead_notes sp-gcs-3)"
want "st=1: log mentions FAIL gate verdict" "closed against a recorded FAIL gate verdict" \
    "$(cat "$TMP/out" 2>/dev/null)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
