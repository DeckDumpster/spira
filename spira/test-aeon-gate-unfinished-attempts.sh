#!/usr/bin/env bash
# test-aeon-gate-unfinished-attempts.sh — the gate-still-running release leaves attempts_of
#   unchanged, the way its own note promises.
#
# THE DEFECT THIS REPRODUCES. aeon.sh's gate-unfinished release tells the bead "No attempt
# was charged" but wrote no net-zero event to back that up: attempts_of nets out only
# `requeued` events whose new_value is 'thrash' or 'unjudged%' (lib.sh _attempts_sql_query),
# and this release path called bump_requeue for none of them. The claim that started the
# session stayed on the events trail unanswered, so every gate-still-running release counted
# toward the poison threshold exactly like a genuine failure.
#
# SEEN RED FIRST: before the fix, the claim aeon.sh makes to work this bead is never offset,
# so attempts_of reads 1 after a single gate-unfinished release. The fix makes aeon.sh call
# bump_requeue with the disposition's own unjudged-gate-unfinished cause, which
# _attempts_sql_query already nets against a claim (test-aeon-disposition.sh covers the
# other two release paths with the same defect, capacity and timeout, at the unit level).
#
# tier: T2
# covers: spira/aeon.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-gate-unfinished-attempts
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — attempts_of reads the events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
testdb_up aeongateatt || {
    printf 'SKIP test-aeon-gate-unfinished-attempts: server testdb not available\n' >&2
    exit 77
}
# shellcheck disable=SC1090
. "$HERE/lib.sh"
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
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan}"
FAYTH_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-gate-unfinished-attempts: aeon.sh has no SPIRA_AGENT injection — refusing to run the real model" >&2; exit 1; }

# gate-run.sh stub: always "still running" (exit 2), the shape gate_unfinished() (aeon.sh)
# requires before it will read the bead's gate as still deciding.
cat > "$SPIRA_HOME/gate-run.sh" <<'STUB'
#!/usr/bin/env bash
echo "gate-run: still running for fixture — 10s so far, pid $$"
exit 2
STUB
chmod +x "$SPIRA_HOME/gate-run.sh"

# The shim runs a turn and ends without closing the bead — same as any other session left
# with an open bead, so the only thing distinguishing this run from a genuine failure is the
# gate-run.sh stub above answering "still deciding".
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

seed() {
    testdb_reset
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; "$HERE/aeon.sh" builder > "$TMP/out" 2>&1; }
bead_status() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status", ""))' 2>/dev/null
}
bead_notes() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("notes", "") or "")' 2>/dev/null
}
num() { local v="$1"; printf '%d' "${v:-0}"; }

echo "test-aeon-gate-unfinished-attempts.sh"
echo

seed sp-gua-1
run_aeon

is   "SEEN RED: bead is released, not left claimed" "open" "$(bead_status sp-gua-1)"
notes="$(bead_notes sp-gua-1)"
want "note says the gate was still running"  "still running" "$notes"
want "note says no attempt was charged"      "No attempt was charged" "$notes"

# THE ACTUAL PROMISE: the claim aeon.sh made to work this bead must be answered by a
# net-zero event, or the note above is a lie the count does not keep.
is "attempts_of reads 0 after a gate-still-running release" "0" "$(num "$(attempts_of sp-gua-1)")"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
