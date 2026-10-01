#!/usr/bin/env bash
# test-aeon-slain-attempts.sh — an aeon-side slain leaves attempts_of unchanged, the way its
#   own note promises.
#
# THE DEFECT THIS REPRODUCES (sp-kp1fl). aeon.sh's slain release logs "no attempt charged"
# but wrote no net-zero event to back that up: attempts_of nets out only `requeued` events
# whose new_value is 'thrash' or 'unjudged%' (lib.sh _attempts_sql_query), and this release
# path called bump_requeue for none of them. The claim that started the session stayed on the
# events trail unanswered, so every slain release counted toward the poison threshold exactly
# like a genuine failure — the same class of gap test-aeon-gate-unfinished-attempts.sh closed
# for gate-unfinished (capacity and timeout already call bump_requeue with their own
# unjudged-<cause>, covered at the unit level by `cargo test -p aeon decide::tests::
# disposition_table`).
#
# SEEN RED FIRST: before the fix, attempts_of read 1 after a single slain release. The fix
# makes aeon.sh call bump_requeue with the disposition's own unjudged-slain cause.
#
# tier: T2
# covers: aeon/src/* spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1090
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-slain-attempts
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — attempts_of reads the events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
testdb_up aeonslainatt || {
    printf 'SKIP test-aeon-slain-attempts: server testdb not available\n' >&2
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
# conf.d IS COPIED IN (matching test-aeon-sweep.sh, test-aeon-world-stop.sh, ...): aeon's
# own in-process config registry (spira_config::resolve, aeon::conf::merge_resolved_config)
# derives conf.d from THIS --home and now REFUSES to start if it is missing (sp-1cdgq) --
# a --home with no conf.d used to resolve silently to nothing instead of refusing.
cp -r "$HERE/conf.d" "$SPIRA_HOME/"
printf '. "%s/lib.sh"\n' "$HERE" > "$SPIRA_HOME/lib.sh"   # the aeon binary sources <home>/lib.sh; this is the real one, as aeon.sh sourced it
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
command -v aeon >/dev/null 2>&1 \
    || { echo "test-aeon-slain-attempts: aeon is not on PATH — refusing to run the real model" >&2; exit 1; }

# The shim runs a turn and ends without closing the bead — same as any other session left
# with an open bead, so the only thing distinguishing this run from a genuine failure is the
# .slain marker planted below, standing in for an operator's slay.sh mid-session.
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
# The .slain marker as slay.sh writes it (date TAB reason) — cleanup() checks only that the
# file exists, so pre-planting it stands in for an operator slaying this session mid-run.
plant_slain_marker() { printf '%s\tslain by operator\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$SPIRA_RUN/$1.slain"; }
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; PATH="$SPIRA_HOME:$PATH" aeon --home "$SPIRA_HOME" builder > "$TMP/out" 2>&1; }
bead_status() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status", ""))' 2>/dev/null
}
num() { local v="$1"; printf '%d' "${v:-0}"; }

seed sp-sla-1
plant_slain_marker sp-sla-1
run_aeon

is   "SEEN RED: bead is released, not left claimed" "open" "$(bead_status sp-sla-1)"

# THE ACTUAL PROMISE: the claim aeon.sh made to work this bead must be answered by a
# net-zero event, or "no attempt charged" in the log is a lie the count does not keep.
is "attempts_of reads 0 after a slain release" "0" "$(num "$(attempts_of sp-sla-1)")"

tl_summary
