#!/usr/bin/env bash
#
# test-aeon-teardown-e2e.sh — one shared full-aeon fixture (full-aeon-fixture.sh), one row
# per open-bead teardown path that only a real aeon.sh run can prove. Everything that does
# NOT need a live session — the disposition precedence table, the spend parser, the
# yield-headless phrasing table, open_ask_blocker's table — already lives as Rust unit tests
# in the aeon crate (decide::tests::{disposition_table, session_yield_headless_table,
# open_ask_blocker_table}, ledger::tests::session_fields_sum_and_last; `cargo test -p aeon`).
# What is left here is the WIRING: that aeon's own marks, the work broker's ask hold, and
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
# covers: aeon/src/* spira/lib.sh mail/src/* spira-lc/src/work.rs work/* cockpit/ops/src/resolve.rs cockpit/ops/src/resolve_main.rs UC-aeon-execution-02 UC-aeon-execution-11 UC-aeon-execution-18
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
. "$HERE/testlib/teardown-e2e.sh"

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
# THE REBASE-CONFLICT REOPEN AT CLOSE (sp-rq-1) IS GONE with sp-v62vn: every session runs
# restricted, and a restricted session hands its bead on only through the work verbs —
# teardown's closed branch (aeon decide::builder_closed, "never took this path and still
# does not") is where the rebase check at close lived, so a session's close now reads as
# submitted and the conflict is the gate's and the landing pass's to find. The row asserted
# a path no session reaches; it is deleted, not rewritten.

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
# The backoff is a timed `wait` hold on the lifecycle row (reason snooze-until:<epoch>), which
# claim reads and which expires by itself; the aeon writes neither a bd status nor a bd defer.
holds_np="$(grep '^sp-np-1 ' "$SPIRA_RUN/lc-holds.log" 2>/dev/null)"
want   "held for the backoff — a wait hold carrying its expiry" "sp-np-1 wait snooze-until:" "$holds_np"
is     "bd status is untouched by the hold" "open" "$(field sp-np-1 status)"
is     "no bd defer is recorded" "" "$(field sp-np-1 defer_until)"
is   "POSITIVE CONTROL — bead not closed, claude rc=1 — aeon exits non-zero" "1" "$rc"

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
timeout 5 git -C "$PSD_REPO" fetch -q origin 2>/dev/null
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
tl_config SPIRA_REPO_MAP="$PSD_REPO_MAP"
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
tl_config SPIRA_REPO_MAP="$FA_REPO_MAP"

tl_summary
