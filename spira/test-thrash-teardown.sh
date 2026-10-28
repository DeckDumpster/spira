#!/usr/bin/env bash
# test-thrash-teardown.sh — G2: the thrash-requeue teardown branch in dwarf.sh's cleanup(),
#   driven through the real dwarf.sh rather than asserted by line-order/awk greps.
#
# What stays here is the one property only a real process can show: a real SIGTERM to the
# process group mid-thrash runs cleanup() once, not twice (CASE 3 below). The thrash dispositions
# themselves (first requeue exempt, streak at the cap charged, the sticking-point note) are
# dwarf::tests (`cargo test -p dwarf thrash`).
#
# defect: sp-4rzlw
# tier: T2
# covers: dwarf/src/* spira/lib.sh UC-dwarf-execution-11
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-thrash-teardown
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up thrash-teardown || { bail "could not build fixture database"; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
# The call jitter is a random sleep before the session starts; CASE 3 signals the group
# once the shim is hung, so the race is removed by pinning it to 0, not waited out.
export SPIRA_CALL_JITTER=0
tl_config SPIRA_CALL_JITTER=0

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; timeout 5 git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; timeout 5 git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/runes"
# round 2 fix (pattern 6): SPIRA_RUNES_DIR no longer derives from SPIRA_HOME — the complete
# fixture declares its own /fixture/userhome/.../runes. Declare this suite's real one.
tl_config SPIRA_RUNES_DIR="$SPIRA_HOME/runes"
printf '. "%s/lib.sh"\n' "$HERE" > "$SPIRA_HOME/lib.sh"   # the dwarf binary sources <home>/lib.sh; this is the real one, as dwarf.sh sourced it
# conf.d IS COPIED IN (matching test-dwarf-sweep.sh, test-dwarf-world-stop.sh,
# test-dwarf-elastic-concurrency.sh, ...): since wave 4.8 (sp-mz7dn), dwarf resolves every
# RETIRED_SNAPSHOT_VARS key (SPIRA_CALL_JITTER among them, sp-1cdgq) through the in-process
# spira_config::resolve() registry pass (dwarf::conf::merge_resolved_config), which derives
# conf.d from THIS --home, not — the way the old bash seam's conf.sh derived it — from
# wherever lib.sh/conf.sh actually live. A --home with no conf.d resolves the whole generic
# registry to nothing, so SPIRA_CALL_JITTER=0 (exported above) never reaches dwarf's Conf
# despite being set in the environment: the jitter falls back to its 20s default regardless,
# reopening sp-1cdgq's own regression through the config seam instead of the bash allowlist.
cp -r "$HERE/conf.d" "$SPIRA_HOME/"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_REPO_MAP="$SPIRA_REPO_MAP"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
# round 2 fix: the complete fixture declares scope_label="spira" as its base value, so
# builder.rune's RUNE_LABELS (resolved against the real config, not this shell's unset
# $SPIRA_SCOPE_LABEL) would require a "spira" label the seeded beads never carry — nothing
# would ever be ready. Declare the empty scope this suite has always meant.
tl_config SPIRA_SCOPE_LABEL=""
cat > "$SPIRA_HOME/runes/builder.rune" <<RUNE
RUNE_NAME=builder
RUNE_WORKING_EXITS="SUBMITTED ask park hold"
RUNE_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
RUNE_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
RUNE_MAX_CONCURRENT=1
RUNE_HEARTBEAT_SECONDS=600
RUNE
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/runes/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
tl_config SPIRA_AGENT="$SPIRA_AGENT"
# The model session is restricted (sp-v62vn); the shim is a fixture — testlib dwarf_fixture_agent.
dwarf_fixture_agent "$BIN/claude"
# The lifecycle machine (testlib lc_dwarf_mirror): since sp-v62vn the dwarf's ready set is
# `spira-lc list` and its claim a Claim event; the stand-in tells the fixture's bd story in
# lifecycle terms, ahead of the tree's spira-lc on PATH.
lc_dwarf_mirror "$TMP/lc"; export PATH="$TMP/lc:$PATH"
command -v dwarf >/dev/null 2>&1 \
    || bail "dwarf is not on PATH — refusing to run the real model"

# Shim C: records the disposition, then hangs — standing in for a session the real heartbeat
# hasn't killed YET, so a real `kill -TERM` sent to the process group from outside (below)
# reaches dwarf.sh itself as a signal, not as cleanup() running off the natural EXIT path.
{ printf '#!/usr/bin/env bash\nRUN_DIR=%q\n' "$SPIRA_RUN"; cat <<'SHIM'
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
if [ -n "${BEAD_ID:-}" ]; then
    SPIRA_RUN="$RUN_DIR" "$TMP/lc/spira-lc" disposition "$BEAD_ID" thrash "stalled: no last action" heartbeat
    : > "$RUN_DIR/$BEAD_ID.hung"
fi
sleep 300
SHIM
} > "$BIN/claude-thrash-hang"
chmod +x "$BIN/claude-thrash-hang"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}
bead_status() {
    # batch-job: fixture bd call against the suite's throwaway store
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status", ""))' 2>/dev/null
}
bead_notes() {
    # batch-job: fixture bd call against the suite's throwaway store
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("notes", "") or "")' 2>/dev/null
}
ledger_count() { grep -c " done builder $1 " "$SPIRA_RUN/dwarf-ledger.log" 2>/dev/null; }
fresh() { testdb_reset; }

echo "test-thrash-teardown.sh"

# ======================================================================================
# CASE 3: a real SIGTERM to the process group mid-thrash must not re-enter cleanup() and
# write a second, contradictory requeue event (sp-u2ve1). cleanup() runs once as the TERM
# handler, takes the thrash branch, and its own `exit $rc` — from a signal handler, not the
# natural EXIT path — used to fire the EXIT trap a second time in the SAME process, with the
# marker already gone, falling through to a second bump_requeue as "unjudged-killed".
# ======================================================================================
echo
echo "real SIGTERM to the process group mid-thrash — exactly one requeue event (sp-u2ve1)"

ln -sf "$BIN/claude-thrash-hang" "$BIN/claude"
fresh; seed sp-tt-3
rm -rf "$SPIRA_RUN/worktree"
marker="$SPIRA_RUN/sp-tt-3.hung"
rm -f "$marker"
setsid dwarf --home "$SPIRA_HOME" builder > "$TMP/out3" 2>&1 &
dwarf_pid=$!

_waited=0
while [ ! -f "$marker" ] && [ "$_waited" -lt 600 ]; do sleep 0.1; _waited=$((_waited + 1)); done
if [ ! -f "$marker" ]; then
    bad "thrash marker appeared before signalling" "not found after 60s"
    kill -TERM -"$dwarf_pid" 2>/dev/null
else
    kill -TERM -"$dwarf_pid" 2>/dev/null
fi

_waited=0
while kill -0 "$dwarf_pid" 2>/dev/null && [ "$_waited" -lt 600 ]; do sleep 0.1; _waited=$((_waited + 1)); done
wait "$dwarf_pid" 2>/dev/null

is    "bead is still open (requeued by the signal-delivered teardown)" "open" "$(bead_status sp-tt-3)"
is    "exactly ONE ledger done-line for this run — one teardown, not two" "1" "$(ledger_count sp-tt-3)"
notes3="$(bead_notes sp-tt-3)"
want   "note says Requeued (thrash)"                       "Requeued (thrash)" "$notes3"
nowant "note is NOT ALSO contaminated by the fallthrough's Not judged" "Not judged" "$notes3"

# ======================================================================================
# Structural: the thrash trip kills the process group (-$$), not just the parent shell —
# a kill to $$ alone defers behind the running claude session (bash defers TERM while
# waiting on a foreground process), so the trip would not actually stop it in time.
# ======================================================================================
echo
# RETIRED with dwarf.sh: the process-group kill is now the Rust dwarf's, pinned by
# `cargo test -p dwarf session::tests::trip_signals_session_group`.
echo "(structural thrash-kill check retired — cargo test -p dwarf session::tests::trip_signals_session_group)"

tl_summary
