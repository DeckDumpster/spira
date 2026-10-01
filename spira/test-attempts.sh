#!/usr/bin/env bash
#
# test-attempts.sh — what counts as an attempt at the work, and what only counts as a worker
# that died.
#
#   ./test-attempts.sh
#
# THE FAILURE THIS SUITE EXISTS FOR. Beads were poisoned without their work having been tried
# once. Three separate defects composed into it, and each has a case below:
#
#   1. `bd unclaim --if-assignee` was passed the FAYTH's name while the claim records the
#      AEON's, so the compare-and-swap could never match and a dying aeon never released its
#      bead.
#   2. The teardown ran under `set -e`, so that failing unclaim ended the shell inside its own
#      EXIT trap — after the counter had been bumped, before anything was logged.
#   3. strand.sh then reclaimed the ghost and bumped the SAME counter again. One death, two of
#      the three attempts, and the third summon poisoned the bead.
#
# The counter is now two counters, which is the property under test throughout: poison must
# measure the work and nothing else. And the charging rule is default-DENY — only an outcome
# that names what the WORK did wrong may charge — because every failure mode that cost the
# most was unenumerated when it fired, so a list of exemptions could not have saved any of
# them.
#
# A REAL bd ON A THROWAWAY DATABASE, because every claim here is a claim about what bd does
# with a label, a lease and a compare-and-swap. A stub would be a second implementation of the
# one thing being asked about (law-prefer-the-real-dependency).
#
# defect: sp-sc3 sp-qd2ul
# tier: T2
# covers: spira/*.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# ======================================================================================
# session_outcome — what ended this session? RETIRED from lib.sh (wave 4.33, sp-8kqww: zero
# live callers — aeon was the only caller, and it calls natively now). It is
# aeon::decide::session_outcome (aeon/src/decide.rs), over an already-extracted trace
# segment (aeon::ledger::trace_segment); every fixture in this block's old table (empty,
# missing, rate-limited, worked-then-refused, clean, api_error_status:null, killed,
# truncated, the appended-segments pair) is `decide::tests::session_outcome_table`. The
# charging rule itself (outcome_charges) was already retired at sp-j89pd (wave 4.2) and is
# exercised by `decide::tests::disposition_table`.
#
# TMP/SPIRA_DB/SPIRA_RUN and the lib.sh source below are kept: the real-bd counters section
# further down still needs them.
# ======================================================================================
TMP="$(mktemp -d)"
export SPIRA_DB="${SPIRA_DB:-$TMP/no-such-db}" SPIRA_RUN="$TMP/run"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

# ======================================================================================
# THE TEARDOWN MUST RUN TO ITS END. Structural, and deliberately so: the regression was that
# a LATER line failed, so nothing short of reading the first line proves the guard is where it
# has to be. The behaviour it guards is exercised right below it.
# ======================================================================================
echo
echo "aeon teardown:"

# RETIRED with aeon.sh: the teardown is the Rust aeon's (no errexit trap exists to disarm);
# its run-to-the-end behaviour is `cargo test -p aeon tests::*` (e.g. slain_mid_session_is_free_and_exits_143).

# The hazard itself, so the assertion above is not a rule nobody can see fire: under `set -e`
# a failing command inside an EXIT trap ends the shell where it stands, and every later step
# of the teardown is skipped in silence.
cat > "$TMP/hazard.sh" <<'H'
set -uo pipefail
cleanup() { echo ENTERED; false; echo LEDGER; }
trap cleanup EXIT
set -e
H
is "errexit ends a trap mid-teardown" "ENTERED" "$(bash "$TMP/hazard.sh" 2>/dev/null)"
cat > "$TMP/guarded.sh" <<'H'
set -uo pipefail
cleanup() { set +e; echo ENTERED; false; echo LEDGER; }
trap cleanup EXIT
set -e
H
is "set +e first lets it finish" "ENTERED
LEDGER" "$(bash "$TMP/guarded.sh" 2>/dev/null)"

# ======================================================================================
# COUNTER LABELS DELETED (sp-lzt). Attempts are now computed from the events trail —
# each status_changed event with new_value containing 'in_progress' is one attempt.
# No harness script writes sp-attempt-*, sp-reclaim-*, sp-requeue-*, sp-timeout-*, or
# sp-recur-* labels. The bump_* functions are no-ops.
#
# The ban-on-writing-a-counter-label check (D6) lives once, in test-attempts-sql.sh; what
# stays here is the other structural property this file is about:
#   The REQUEUE_CAUSE path in aeon.sh still exits before session_outcome is consulted —
#   a reopened bead is not charged an attempt.
# ======================================================================================
echo
echo "counter labels deleted — structural properties:"

# The REQUEUE_CAUSE exemption must still be decided before session_outcome is consulted.
# A bead that was put back by the harness must not be charged an attempt.
# RETIRED with aeon.sh: the requeue-before-outcome order is the Rust aeon's disposition
# table, pinned by `cargo test -p aeon decide::tests::disposition_table`.

# BEHAVIOUR, NOT THE QUERY STRING. The original assertion here checked that the SQL
# contained the word 'status_changed'. It passed while the predicate returned 0 for every
# bead an aeon had ever worked, because an aeon claim writes event_type='claimed' and only
# a hand-driven `bd update --status in_progress` writes 'status_changed'. A test that
# restates the implementation agrees with it about everything, including its mistakes.
#
# So: run the query against a fixture holding one of each event shape and assert the NUMBER.
body_sql="$(sed -n '/^_attempts_sql_query()/,/^}/p' "$HERE/lib.sh" 2>/dev/null)"
is "attempts counts an aeon claim" "1" \
   "$(grep -c "event_type='claimed'" <<<"$body_sql" || true)"
is "attempts also counts a hand-driven in_progress transition" "1" \
   "$(grep -c 'status_changed' <<<"$body_sql" || true)"

# THE b1..b8 FIXTURE (hand-rolled dolt schema, one row per attempt-counting scenario:
# harness-requeue vs genuine failure, thrash pairs, unjudged deaths) moved to
# test-attempts-sql.sh (sp-eq8a4.2.4), seeded through real bd events instead of a copy of
# the events schema — the same fixture, testing the real store rather than a model of it.
#
# The not-judged branch's "records an unjudged requeue event" was a source-order grep of
# aeon.sh; sp-eq8a4.2.1 moved that cause into aeon_disposition, then sp-j89pd (wave 4.2)
# moved it again into aeon::decide::disposition (aeon/src/decide.rs), where the
# unjudged-<cause> rows (gap G15) of its own `disposition_table` unit test assert it.

# ======================================================================================
# The counters and the release, against a real bd.
# ======================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-attempts
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — attempts_of (its own subject) reads the events table via bd sql, which embedded mode refuses
export SPIRA_TESTDB_MODE=server
testdb_up attempts || {
    printf 'SKIP test-attempts: server testdb not available\n' >&2
    exit 77
}

seed() {   # seed <id> — one open, claimable bead
    testdb_reset
    testdb_seed <<JSONL
{"id":"$1","title":"a bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
JSONL
}
status_of() { bdjson show "$1" | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("status","") if d else "")' 2>/dev/null; }
num() { local v="$1"; printf '%d' "${v:-0}"; }
# reclaims_of (lib.sh) was retired at sp-8itaf — zero live callers (the production
# accessor is now strand::check::reclaims_of, Rust). _counter_events_query, the shared
# helper it and requeues_of both used, stays — requeues_of still calls it below — so this
# reaches the identical count the old function did.
reclaims_of() { _counter_events_query "${1:-}" reclaimed; }

echo
echo "counters (real bd) — events-based, no labels written:"

seed sp-c1
# THE POSITIVE CONTROL. A fresh bead with no status changes has zero attempts, zero
# reclaims and zero requeues. reclaims_of/requeues_of count real events (test-attempts-sql.sh
# b10 proves a reclaimed event counts); timeouts_of alone is a hardcoded-0 stub.
is "a fresh bead has no attempts"       0 "$(num "$(attempts_of sp-c1)")"
is "a fresh bead has no reclaims"       0 "$(num "$(reclaims_of sp-c1)")"
is "a fresh bead has no requeues"       0 "$(num "$(requeues_of sp-c1)")"


# The single-transition case (one in_progress = 1 attempt) is test-attempts-sql.sh's sp-ev2;
# what stays here is the multi-event, real-CLI-driven complement to that file's SQL-seeded
# b3/b4 fixture: genuine failure still poisons via events, three in_progress events reach
# the threshold.
poisons() { local n; n="$(num "$(attempts_of "$1")")"; [ "$n" -ge 3 ] && echo yes || echo no; }
seed sp-c2
bdq update sp-c2 --status in_progress >/dev/null 2>&1
bdq update sp-c2 --status open >/dev/null 2>&1
bdq update sp-c2 --status in_progress >/dev/null 2>&1
bdq update sp-c2 --status open >/dev/null 2>&1
is "two events do not poison yet" no "$(poisons sp-c2)"
bdq update sp-c2 --status in_progress >/dev/null 2>&1
is "three events poison" yes "$(poisons sp-c2)"

# `attempts.sh reclassify`/`prune-reclaims` (the sp-attempt-N/sp-reclaim-N-unrecorded LABEL
# cleanup) and `attempts.sh clear` were tested here until sp-rfodk, when attempts.sh moved
# into spira-claim. reclassify/prune-reclaims are retired, not ported: `bump_counter` stopped
# writing those labels at sp-lzt, and a read-only scan of the live store on 2026-09-30 found
# zero `sp-attempt-N-*` or `sp-reclaim-N-unrecorded` labels anywhere in it — nothing has been
# a candidate for either command since the day they stopped being written, so there is no
# behaviour left to carry forward. `clear`'s job (lift a poison and make it stick, sp-qd2ul)
# is `spira-claim unpoison`'s job now, and superseded attempts.sh's `clear` before this bead —
# its coverage, including the real spira-lc/Dolt server this suite used to stand up only for
# that block, is spira-claim's own `cargo test -p spira-claim` (unpoison.rs) plus the real
# store in test-unpoison.sh.

echo
echo "the release (real bd):"

# THE DISCRIMINATING FACT, seen both ways. The claim records BEADS_ACTOR; the teardown must
# name that same actor or the compare-and-swap can never match.
seed sp-r1
BEADS_ACTOR=aeon-cindy bdq update sp-r1 --claim >/dev/null 2>&1
is "the claim records the aeon, not the fayth" in_progress "$(status_of sp-r1)"

if bdq unclaim sp-r1 --if-assignee aeon-builder >/dev/null 2>&1; then r=0; else r=1; fi
is "releasing as the fayth fails"        1           "$r"
is "and leaves the bead held"            in_progress "$(status_of sp-r1)"

if bdq unclaim sp-r1 --if-assignee aeon-cindy >/dev/null 2>&1; then r=0; else r=1; fi
is "releasing as the aeon succeeds"      0    "$r"
is "and the bead is claimable again"     open "$(status_of sp-r1)"
tl_summary
