#!/usr/bin/env bash
#
# test-aeon-teardown-e2e.sh — one shared full-aeon fixture (full-aeon-fixture.sh), one row
# per open-bead teardown path that only a real aeon.sh run can prove. Everything that does
# NOT need a live session — the disposition precedence table, the spend parser, the
# yield-headless phrasing table, open_ask_blocker's table — already lives as Rust unit tests
# in the aeon crate (decide::tests::{disposition_table, session_yield_headless_table,
# open_ask_blocker_table}, ledger::tests::session_fields_sum_and_last; `cargo test -p aeon`).
# What is left here is the WIRING: that aeon's own marks, mail's own marker, and
# attempts_of's own SQL agree with what those tables predict.
#
# Replaces seven suites (docs/test-plan/aeon-execution.md D8, D12, D15 — sp-g44ke):
# test-aeon-decision-blocked.sh, test-aeon-operator-wait.sh, test-requeue.sh,
# test-aeon-ledger.sh, test-aeon-presession-death.sh, test-aeon-yield-headless.sh,
# test-aeon-exit.sh — ~1,460 lines, each building its own bare origin, clone, chamber and
# claude shim to assert one `aeon-ledger.log status=` line apiece. Every one of those
# suites' use cases is a row below; test-requeue.sh's deadlock-sweep half (UC-24, no aeon
# run) moved to test-deadlock-sweep.sh instead, since it never touched a live session.
#
# ONE FIXTURE, RESET BETWEEN ROWS (fa_reset), never rebuilt: the git and chamber setup is
# built once by full-aeon-fixture.sh's fa_setup, which is the D15 saving — 20 suites each
# paid for `git init --bare` + clone + chamber write-out on every one of their own runs.
#
# SERVER-MODE bd, for the whole file: the rebase-conflict row's attempts_of/requeues_of
# read the events table via `bd sql`, which embedded mode refuses (testdb.sh), and mode is
# a property of the whole store, not chosen per call.
#
# defect: sp-egge2 sp-ne93n sp-l7f5 sp-214 sp-ywlti sp-iu10 sp-2a4hd sp-wnsks
# tier: T3
# covers: aeon/src/* spira/lib.sh mail/src/* UC-aeon-execution-02 UC-aeon-execution-11 UC-aeon-execution-12 UC-aeon-execution-18
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# testdb-mode: server — the rebase-conflict row's attempts_of/requeues_of read the events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/full-aeon-fixture.sh"

fa_setup teardowne2e || exit 77
trap 'fa_teardown' EXIT INT TERM

echo "test-aeon-teardown-e2e.sh"

# ==========================================================================================
echo
echo "ROW: session did not close (charged) / rebase-conflict reopen (requeued, not charged)"
# ==========================================================================================
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control): "no attempt was charged" is
# the answer a counter that never runs gives too, so the decline below sits beside a charge
# the same code path produces from the same fixture.
#
# THE REBASE CONFLICT IS REAL, not simulated by a flag: the base gains a commit touching the
# same line the shim's commit touches, which is what the aeon's own rebase step then fails
# on — a fixture that set the outcome directly would be asserting against a model of the
# thing under test.
MAINREPO="$FA_REPO"; export MAINREPO
shim() {   # shim <commit:0|1> <close:0|1> [move-the-base:0|1] [claude-rc:0|1]
    printf '%s' "$1" > "$FA_TMP/docommit"; printf '%s' "$2" > "$FA_TMP/doclose"
    printf '%s' "${3:-0}" > "$FA_TMP/domove"; printf '%s' "${4:-0}" > "$FA_TMP/doclauderc"
    cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
if [ "$(cat "$TMP/docommit")" = 1 ]; then
    printf 'the aeon wrote this %s\n' "$(date +%s%N)" > f
    git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
fi
# THE BASE MOVES WHILE THE SESSION IS RUNNING, which is the live shape and the only one that
# produces the defect: a base that had already moved before the aeon started would simply be
# branched from, and there would be nothing to rebase.
if [ "$(cat "$TMP/domove")" = 1 ]; then
    git -C "$MAINREPO" checkout -q main
    printf 'someone else landed this %s\n' "$(date +%s%N)" > "$MAINREPO/f"
    git -C "$MAINREPO" -c user.email=b@b -c user.name=other commit -qam "another bead — a conflicting change"
    git -C "$MAINREPO" push -q origin main 2>/dev/null
fi
[ "$(cat "$TMP/doclose")" = 1 ] && bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit "$(cat "$TMP/doclauderc")"
SHIM
    chmod +x "$FA_BIN/claude"
}
field() { fa_field "$1" "$2"; }
lib() { bash -c ". \"$HERE/lib.sh\"; $1" 2>/dev/null; }
count_of()   { local c; c="$(lib "attempts_of $1")"; printf '%s' "${c:-0}"; }
requeue_of() { local c; c="$(lib "requeues_of $1")"; printf '%s' "${c:-0}"; }

fa_reset; fa_seed sp-rq-1; shim 1 1 1; fa_run_aeon >/dev/null
is     "reopened over a rebase conflict — the bead is open again" open "$(field sp-rq-1 status)"
want   "and the log says why"                  "REOPENED — closed behind" "$(fa_out)"
is     "no attempt is charged"                 "0" "$(count_of sp-rq-1)"
is     "it is counted as a requeue instead"    "1" "$(requeue_of sp-rq-1)"
want   "the teardown says no attempt was charged" "no attempt charged" "$(fa_out)"
want   "the bead carries the decision"         "Requeue 1 (rebase-conflict)" "$(fa_notes sp-rq-1 | tr -s ' ')"
want   "and the ledger carries the outcome"    "status=requeue-rebase-conflict" "$(fa_ledger_line sp-rq-1)"
# THE BRANCH SURVIVES THE REQUEUE. The aeon committed before closing; the rebase failed
# after close and was aborted, leaving the branch at its pre-abort tip.
_nc="$(git -C "$FA_REPO" rev-list --count "$(git -C "$FA_REPO" rev-parse origin/main)..spira/sp-rq-1" 2>/dev/null || echo 0)"
is     "the branch still carries the aeon's commit after the requeue" "1" "$_nc"

# ALSO the exit-code positive control (UC-18): a bead left open with claude's own rc=1 must
# make aeon itself exit non-zero — reusing this run rather than a dedicated one, since the
# fix side of the same UC (a CLOSED bead, claude rc=1, aeon exits 0) still gets its own row
# below where a stray non-zero exit is the whole point.
#
# COMMITS this time (sp-1zxru): a real failed attempt — the session moved the branch and
# still left the bead open — still charges. The no-commit shape of this exact shim call
# (commit=0) is its own row below, since sp-1zxru that shape is a no-progress exit instead.
fa_reset; fa_seed sp-rq-2; shim 1 0 0 1; rc="$(fa_run_aeon)"
is   "session did not close the bead — bead is open" open "$(field sp-rq-2 status)"
is   "and one attempt IS charged (the normal unlanded case)" "1" "$(count_of sp-rq-2)"
is   "with nothing on the requeue counter"     "0" "$(requeue_of sp-rq-2)"
want "and the note reads Unlanded"             "Unlanded" "$(fa_notes sp-rq-2)"
is   "POSITIVE CONTROL — bead not closed, claude rc=1 — aeon exits non-zero" "1" "$rc"

# ==========================================================================================
echo
echo "ROW: no commit, left open — no-progress exit, held for a backoff, not charged (sp-1zxru)"
# ==========================================================================================
# THE DEFECT THIS GUARDS: aeon-ledger.log since 2026-10-02T07:36Z showed sp-6a4rb/sp-0k18y/
# etc resumed every ~75s by the SAME aeon because a session that left the bead in_progress
# with no commit was charged and immediately reclaimable exactly like a real failed attempt
# — beads got poisoned and round-duty's attempts ask flooded Ryan's inbox over what is a
# harness loop, not a judged attempt (law-attempts-count-the-harness). Same shim shape
# sp-rq-2 used before this bead (commit=0, close=0, claude-rc=1) — no commit is the whole
# point this time.
fa_reset; fa_seed sp-np-1; shim 0 0 0 1; rc="$(fa_run_aeon)"
is   "no attempt is charged" "0" "$(count_of sp-np-1)"
# requeue_of is the RAW count of 'requeued' events (lib.sh's requeues_of ->
# spira-claim count-events, no exemption) — every other unjudged-* disposition
# (capacity, slain, gate-unfinished, decision-blocked, timeout, NotJudged) also calls
# bump_requeue and would show the same 1 here; it is attempts_of (the exemption-aware
# fold, asserted above as 0) that the poison threshold and CHECK 4's ask actually read.
is   "one raw requeued event (the exempt bump_requeue cause), same as every other unjudged-* disposition" \
     "1" "$(requeue_of sp-np-1)"
notes_np="$(fa_notes sp-np-1)"
want   "the note reads No progress, not Unlanded"   "No progress" "$notes_np"
nowant "the note does not read Unlanded"            "Unlanded"    "$notes_np"
want   "the note says no attempt was charged"       "No attempt charged" "$notes_np"
# sp-k7eqd: status=deferred hid expired holds from bd ready forever (27 beads stranded) —
# the fix leaves status OPEN and relies on a future defer_until to keep bd ready from
# listing it until the backoff elapses, so it releases itself instead of needing a second
# actor to flip it back. "held" now means open + defer_until, not status=deferred.
is     "held for the backoff — status stays open so it releases itself" \
       "open" "$(field sp-np-1 status)"
[ -n "$(field sp-np-1 defer_until)" ] && _defer_set=yes || _defer_set=no
is   "a defer_until is recorded — the hold the next ready query reads" "yes" "$_defer_set"
np_ready="$(bd -C "$SPIRA_DB" ready --limit 0 --exclude-type epic,event -u --json 2>/dev/null)"
nowant "held — open + a future defer_until still keeps it off bd ready, not plain open for the next summon" \
       '"sp-np-1"' "$np_ready"
is   "POSITIVE CONTROL — bead not closed, claude rc=1 — aeon exits non-zero" "1" "$rc"

# ==========================================================================================
echo
echo "ROW: decision-blocked — released, no attempt charged"
# ==========================================================================================
# Shim creates a decision bead blocking the claimed bead, then exits non-zero — simulating an
# aeon that filed a question via mail for a decision bead. The dep is added AFTER the bead
# is claimed (in_progress); bd ready only returns unblocked beads, so a pre-existing dep would
# prevent the claim entirely.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
_bd="${SPIRA_BD:-bd}"
id="$(BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
if [ -n "$id" ]; then
    BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" create \
        "Operator question about $id" \
        -l "${SPIRA_ASK_LABEL:-needs-operator},overseer" \
        --type decision \
        --deps "blocks:$id" \
        --silent >/dev/null 2>&1 || true
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 1
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-db-2; fa_run_aeon >/dev/null
is "bead is still open (correctly not closed)" "open" "$(fa_status sp-db-2)"
notes2="$(fa_notes sp-db-2)"
want "note says released due to decision blocker" "decision" "$notes2"
want "note says no attempt charged" "No attempt charged" "$notes2"
nowant "note does not say Unlanded" "Unlanded" "$notes2"
want "ledger says decision-blocked" "decision-blocked" "$(fa_ledger_line sp-db-2)"

# The sp-dvsqc defect (an ask-labelled dep via a relates-to edge treated as a blocker) does
# not get a row here: open_ask_blocker (aeon::decide::open_ask_blocker), the dependency read
# this row's own decision-blocked branch feeds on, is pure and is a table in
# aeon/src/decide.rs instead (`cargo test -p aeon decide::tests::open_ask_blocker_table`) —
# no live session needed, no cost against this file's cap.

# ==========================================================================================
echo
echo "ROW: own issue-closeout ask — not a blocker, attempt IS charged"
# ==========================================================================================
# sp-2a4hd: a bead closed, asked about, and then reopened before its own "Close GitHub
# issue ... for bead <id>" ask was resolved must not be decision-blocked by that ask — it
# must be worked normally.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
_bd="${SPIRA_BD:-bd}"
id="$(BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
if [ -n "$id" ]; then
    # COMMITS (sp-1zxru): this row proves the own-closeout ask is not a decision-blocker,
    # which needs the session to reach the real unlanded/charged path to show — a session
    # with no commit now lands on the no-progress exit instead (its own row covers that
    # shape), which would make the "No attempt charged" nowant below a false positive.
    printf 'the aeon wrote this %s\n' "$(date +%s%N)" > f
    git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
    BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" create \
        "Close GitHub issue github:fixture/testrepo#99 for bead $id" \
        -l "${SPIRA_ASK_LABEL:-needs-operator},overseer" \
        --type decision \
        --deps "blocks:$id" \
        --silent >/dev/null 2>&1 || true
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 1
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-db-4; fa_run_aeon >/dev/null
notes4="$(fa_notes sp-db-4)"
want   "own-closeout-ask: attempt IS charged (Unlanded, not released)" "Unlanded" "$notes4"
nowant "own-closeout-ask: not released as decision-blocked" "No attempt charged" "$notes4"
nowant "own-closeout-ask: ledger must not say decision-blocked" "decision-blocked" "$(fa_ledger_line sp-db-4)"

# ==========================================================================================
echo
echo "ROW: operator-wait marker — released, no attempt charged"
# ==========================================================================================
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
if [ -n "${BEAD_ID:-}" ] && [ -n "${SPIRA_RUN:-}" ]; then
    printf '%s\n' "$SESSION_EPOCH" > "$SPIRA_RUN/$BEAD_ID.operator-wait"
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-ow-2; fa_run_aeon >/dev/null
is   "bead is still open (correctly not closed)" "open" "$(fa_status sp-ow-2)"
notes_ow="$(fa_notes sp-ow-2)"
want "note says kind-question mail"      "kind-question mail" "$notes_ow"
want "note says No attempt charged"      "No attempt charged" "$notes_ow"
nowant "note does not say Unlanded"      "Unlanded"           "$notes_ow"
want "ledger says operator-wait" "operator-wait" "$(fa_ledger_line sp-ow-2)"

echo
echo "mail, driven directly (no aeon run): kind=question writes the operator-wait marker"
# mail is a compiled binary now (sp-ooh1k) — there is no `cmd_send` bash function left to
# source; call the real `send` subcommand instead (bare name, on the suite's own PATH).
BEAD_ID=sp-ow-mail SPIRA_RUN="$SPIRA_RUN" mail send operator --from "Builder <builder@spira>" \
    --subject "fixture question" --kind question --default "proceed without waiting" <<BODY >/dev/null 2>&1
## Question

Can the fixture answer this itself?

## Default

Proceed without waiting.
BODY
is "mail send kind=question wrote the marker itself" "yes" \
   "$([ -e "$SPIRA_RUN/sp-ow-mail.operator-wait" ] && echo yes || echo no)"

# ==========================================================================================
echo
echo "ROW: operator-wait marker present AND bead closed — consumed, not left stranded (sp-nw7jb)"
# ==========================================================================================
# THE DEFECT THIS GUARDS. The disposition gathering that consumes the operator-wait marker
# only runs for an OPEN bead (the `st != closed` branch above) — so a session that sent a
# kind-question mail and then closed its own bead in the same breath never reached the
# branch that removes the marker. It sat on disk forever: no garbage collector, one
# consumer, and that consumer never ran.
#
# COMMITTED, so close_verdict reads "keep|committed" and the close genuinely stands — a
# close with nothing committed is reopened by an entirely different, unrelated mechanism
# (closed-without-commit) that would otherwise obscure what this row is proving. Chore, not
# task: a work-type close is converted to "submitted" by a later, unrelated block regardless
# of commit or this fix, for the same reason.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
_bd="${SPIRA_BD:-bd}"
id="$(BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
if [ -n "$id" ]; then
    printf 'the aeon wrote this %s\n' "$(date +%s%N)" > f
    git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
    printf '%s\n' "$SESSION_EPOCH" > "$SPIRA_RUN/$id.operator-wait"
    BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" close "$id" --reason "done, asked a question in passing" >/dev/null 2>&1
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-ow-3; bd -C "$SPIRA_DB" update sp-ow-3 --type chore >/dev/null 2>&1
fa_run_aeon >/dev/null
is   "the marker does not survive the session that wrote it, even though the bead closed" "no" \
     "$([ -e "$SPIRA_RUN/sp-ow-3.operator-wait" ] && echo yes || echo no)"
is   "the close stands (a non-work type closes by the agent's own hand, unchanged)" "closed" "$(fa_status sp-ow-3)"
want "the close is recorded as carrying an operator-wait marker" \
     "operator-wait marker" "$(fa_notes sp-ow-3)"

# ==========================================================================================
echo
echo "ROW: operator-wait marker from a PREVIOUS session is ignored, not read as this one's wait (sp-nw7jb)"
# ==========================================================================================
# THE DEFECT THIS GUARDS. Before mail stamped the marker with its writing session's own
# SESSION_EPOCH, the marker carried no identity at all — any later session on the same bead
# that exited without closing was released here with NO attempt charged, on the strength of
# a question a DIFFERENT, earlier session asked. Reproduced by pre-seeding a marker stamped
# with an epoch that cannot be this run's (`1`, 1970) before the aeon ever starts.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
# COMMITS (sp-1zxru): this row proves the stale marker is ignored, which needs the real
# unlanded/charged path to show it landed on — a session with no commit now lands on the
# no-progress exit instead (its own row covers that shape).
if [ -n "${BEAD_ID:-}" ]; then
    printf 'the aeon wrote this %s\n' "$(date +%s%N)" > f
    git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$BEAD_ID — the work"
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 1
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-ow-4
printf '1\n' > "$SPIRA_RUN/sp-ow-4.operator-wait"
fa_run_aeon >/dev/null
is   "bead is still open (session did not close)" "open" "$(fa_status sp-ow-4)"
notes_stale="$(fa_notes sp-ow-4)"
want   "charged as the normal unlanded case, not released as operator-wait" "Unlanded" "$notes_stale"
nowant "not released on a stranger's wait" "kind-question mail" "$notes_stale"
nowant "ledger must not say operator-wait" "operator-wait" "$(fa_ledger_line sp-ow-4)"
is   "the stale marker was cleared, not left for the next summon either" "no" \
     "$([ -e "$SPIRA_RUN/sp-ow-4.operator-wait" ] && echo yes || echo no)"
want "the log says the marker predates this session" \
     "operator-wait marker predates this session" "$(fa_out)"

# ==========================================================================================
echo
echo "ROW: pre-session death — worktree failure is named, charged, and the loop is bounded"
# ==========================================================================================
# THE DEFECT THIS GUARDS. An aeon that dies before its Claude session starts used to be
# recorded as a refusal (no attempt charged) and re-summoned forever: the ledger line read
# rc=0 status=in_progress with every metric ?, identical to a short successful run.
#
# A DIFFERENT REPO SHAPE, on purpose: a local REMOTE plus a REPO that tracks it, so
# refs/remotes/origin/main can be deleted to make the land ref unresolvable without
# touching the shared FA_REPO/FA_ORIGIN the other rows use.
PSD_REMOTE="$FA_TMP/psd-remote"
git init -q -b main "$PSD_REMOTE"
git -C "$PSD_REMOTE" config user.email t@t; git -C "$PSD_REMOTE" config user.name t
printf 'seed\n' > "$PSD_REMOTE/f"; git -C "$PSD_REMOTE" add f; git -C "$PSD_REMOTE" commit -qm seed

PSD_REPO="$FA_TMP/psd-repo"
git init -q -b main "$PSD_REPO"
git -C "$PSD_REPO" config user.email t@t; git -C "$PSD_REPO" config user.name t
git -C "$PSD_REPO" remote add origin "$PSD_REMOTE"
git -C "$PSD_REPO" fetch -q origin 2>/dev/null
git -C "$PSD_REPO" checkout -q -b main --track origin/main 2>/dev/null \
    || git -C "$PSD_REPO" checkout -q main 2>/dev/null || true
git -C "$PSD_REPO" remote set-head origin --auto 2>/dev/null || true
git -C "$PSD_REPO" update-ref -d refs/remotes/origin/main

PSD_REPO_MAP="$FA_TMP/psd-repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$PSD_REPO" > "$PSD_REPO_MAP"

# THE SHIM MUST NEVER BE CALLED — the aeon dies before the session starts.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf 'test-aeon-teardown-e2e: shim was invoked — aeon did not die pre-session\n' >&2
exit 1
SHIM
chmod +x "$FA_BIN/claude"

fa_reset; fa_seed sp-pd-1
export SPIRA_REPO_MAP="$PSD_REPO_MAP"
fa_run_aeon >/dev/null
want "error message is logged (not swallowed)"    "land ref cannot be resolved" "$(fa_out)"
want "ledger status is pre-session"               "status=pre-session"          "$(fa_ledger_line sp-pd-1)"
nowant "not recorded as in_progress"              "status=in_progress"          "$(fa_ledger_line sp-pd-1)"
nowant "rc is not 0 (session never ran)"          "rc=0 "                       "$(fa_ledger_line sp-pd-1)"

# The bead is back in open state after cleanup released it. Run again — ref still absent.
fa_run_aeon >/dev/null
_count="$(fa_ledger_lines sp-pd-1 | grep -c 'status=pre-session' 2>/dev/null || echo 0)"
want "second death also charges (ledger-based; the infinite loop is broken)" "status=pre-session" "$(fa_ledger_line sp-pd-1)"
is   "two pre-session entries in the ledger" "2" "$_count"

# The rapid-recur park (a third consecutive sub-10s death labels and parks the bead so a
# fourth summon cannot repeat the same futile retry — SPIRA_RAPID_RECUR_THRESHOLD) does not
# get rows here: rapid_recur_check (aeon::run::Run::rapid_recur_check) needs only a ledger
# file and one fake bd bead, no live aeon.sh session, so it is aeon/src/tests.rs's
# rapid_recur_* tests instead (`cargo test -p aeon tests::rapid_recur`) — two fewer real aeon
# runs against this file's cap for a park behaviour distinct from the FATAL/charge/loop story
# this row proves.

# RESTORE the shared repo-map — every row after this one uses FA_REPO again.
export SPIRA_REPO_MAP="$FA_REPO_MAP"

# ==========================================================================================
echo
echo "ROW: ledger segment boundary — attempt 2 reads its OWN segment, not attempt 1's"
# ==========================================================================================
# What only a real aeon run can prove: that the aeon's own marks and attempt_trace's
# backward scan land on the same boundary a forward scan would find (aeon::ledger). The
# forward-scan half (spira_trace_mark/trace_segment, and session_result_fields's own field
# table) was lib.sh's and retired dead with it (sp-j89pd, wave 4.2) — superseded by
# aeon::ledger::session_fields and its own unit tests.
FULL='{"type":"result","subtype":"success","is_error":false,"duration_ms":90480,"duration_api_ms":61400,"num_turns":7,"total_cost_usd":1.3474715,"usage":{"input_tokens":1234,"cache_creation_input_tokens":105984,"cache_read_input_tokens":456789,"output_tokens":2222,"output_tokens_details":{"thinking_tokens":333}},"result":"done"}'
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
if [ "$(cat "$TMP/docommit")" = 1 ]; then
    printf 'my work\n' >> f
    git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
fi
[ "$(cat "$TMP/doclose")" = 1 ] && bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
cat "$TMP/result"
exit 0
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-lg-5
printf 1 > "$FA_TMP/docommit"; printf 0 > "$FA_TMP/doclose"; printf '%s\n' "$FULL" > "$FA_TMP/result"
fa_run_aeon >/dev/null           # ran, spent, left the bead open
first="$(fa_ledger_line sp-lg-5)"
printf 0 > "$FA_TMP/docommit"; printf 0 > "$FA_TMP/doclose"; : > "$FA_TMP/result"
fa_run_aeon >/dev/null           # never spoke
second="$(fa_ledger_line sp-lg-5)"
want   "attempt 1 is a full reading"          "turns=7"      "$first"
want   "and it cost what the record said"     "cost_usd=1.3475" "$first"
nowant "attempt 2 does not inherit its turns"  "turns=7"     "$second"
nowant "nor its cost"                          "cost_usd=1.3475" "$second"
want   "attempt 2 reads as unknown"            "turns=?"     "$second"
want   "and unknown on the cost"               "cost_usd=?"  "$second"
want   "and the fields ride a disposition that is not a close" "status=in_progress" "$second"

# ==========================================================================================
echo
echo "ROW: yield-headless — ledger status and bead note, wired end to end"
# ==========================================================================================
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo build"}}]}}\n'
printf '{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"The build is running. I will wait for the background task notification to continue."}],"stop_reason":"end_turn"}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":5000,"num_turns":2,"total_cost_usd":0.01}\n'
exit 0
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-yh-1; fa_run_aeon >/dev/null
want "ledger records yield-headless"      "status=yield-headless" "$(fa_ledger_line sp-yh-1)"
want "bead note mentions yield-headless"  "background task notification" "$(fa_notes sp-yh-1)"

# ==========================================================================================
echo
echo "ROW: exit code, bead mode — closed bead exits 0 regardless of claude's own rc"
# ==========================================================================================
# THE DEFECT THIS TESTS. ops and qa run as named systemd units. A named unit enters FAILED
# when its ExecStart exits non-zero — an alert that is always firing is one nobody reads
# (law-alerts-must-be-actionable) — so a session that did the work and closed the bead must
# not fail the unit just because the claude CLI's own exit code was a stray non-zero.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
cat /dev/stdin > /dev/null 2>&1
id="$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
printf 'my work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit "$(cat "$TMP/shim-rc" 2>/dev/null || echo 0)"
SHIM
chmod +x "$FA_BIN/claude"

fa_reset; fa_seed sp-ex-2
printf 1 > "$FA_TMP/shim-rc"
rc="$(fa_run_aeon)"
is "bead is converted to submitted, not left closed" "open" "$(fa_status sp-ex-2)"
is "aeon exits 0 despite claude rc=1 (the fix)" "0" "$rc"
want "ledger still records the real rc" "rc=1" "$(fa_ledger_line sp-ex-2)"
want "and records the submitted status" "status=submitted" "$(fa_ledger_line sp-ex-2)"
# The positive control for this UC (bead not closed, claude rc=1, aeon exits non-zero) is
# the "session did not close" row above (sp-rq-2) — the same discrimination, one fewer run.

# ==========================================================================================
echo
echo "ROW: FAYTH_GRAPH_ONLY persona closes a work bead with no commit — close stands (sp-wnsks)"
# ==========================================================================================
# The groomer's own shape: no Edit or Write in FAYTH_TOOLS, so its close is never followed
# by a commit — sp-yyzm3 (filed by hand, no delivers: label) was converted to submitted
# by the row above's same logic and stranded there forever, since a graph-only edit never
# produces the commit that conversion waits for. FAYTH_GRAPH_ONLY=1 is the fix: the
# conversion above must not fire for this persona, commit or no commit.
cat > "$FA_HOME/chamber/groomonly.fayth" <<GOFAYTH
FAYTH_NAME=groomonly
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH_GRAPH_ONLY=1
GOFAYTH
printf 'groom-only close {{BEAD_ID}}\n{{PARK}}\n' > "$FA_HOME/chamber/groomonly.md"

cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
cat /dev/stdin > /dev/null 2>&1
id="$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" close "$id" --reason "graph-only: dependency re-pointed, nothing to commit" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$FA_BIN/claude"

fa_reset; fa_seed sp-ex-3
rc="$(fa_run_aeon groomonly)"
is "graph-only close stands — NOT converted to submitted" "closed" "$(fa_status sp-ex-3)"
is "aeon exits 0" "0" "$rc"
# fa_ledger_line hardcodes the "builder" fayth name; this row runs as groomonly, so read
# its own last done-line directly rather than duplicating that assumption.
ledger3="$(grep " sp-ex-3 rc=" "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null | tail -1)"
want   "ledger has a done line for this bead" "sp-ex-3" "$ledger3"
nowant "and it does NOT record a submitted conversion" "status=submitted" "$ledger3"
# The positive control for this UC (the same shim, no FAYTH_GRAPH_ONLY) is the "exit code,
# bead mode" row above (sp-ex-2): identical close, converted to submitted without the flag.

# ==========================================================================================
echo
echo "ROW: exit code, sweep mode — claude rc=1 but ran still exits 0"
# ==========================================================================================
cat > "$FA_HOME/chamber/sweeper.fayth" <<SFAYTH
FAYTH_NAME=sweeper
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
SFAYTH
printf 'sweep {{BEAD_ID}}\n{{PARK}}\n' > "$FA_HOME/chamber/sweeper.md"

cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":2000,"num_turns":1,"total_cost_usd":0.001}\n'
exit "$(cat "$TMP/shim-rc" 2>/dev/null || echo 0)"
SHIM
chmod +x "$FA_BIN/claude"
printf 1 > "$FA_TMP/shim-rc"
sweep_rc="$(aeon --home "$SPIRA_HOME" sweeper --sweep --prompt "check pipeline" > "$FA_TMP/sweep-out" 2>&1; echo $?)"
is "sweep with claude rc=1 but ran exits 0 (ops/qa sweep fix)" "0" "$sweep_rc"
# The positive control for this UC (a refused sweep — no tool calls — exits non-zero so a
# real ops failure stays visible) is test-aeon-sweep.sh's instead, which already builds the
# lighter sweep-only fixture this control needs and does not touch this file's 60s cap.

tl_summary
