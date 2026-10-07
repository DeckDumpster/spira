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
lc_facts_stub "$TMP/lc"
export SPIRA_DB="${SPIRA_DB:-$TMP/no-such-db}"
tl_config SPIRA_RUN="$TMP/run"
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
#
# _attempts_sql_query (lib.sh) was PORTED to spira-claim at wave 4.18 (sp-sn1re):
# attempts_of is now a one-line shim onto `spira-claim attempts`, so there is no SQL
# source text left in lib.sh to grep for 'claimed'/'status_changed' — the same body-text
# trap this comment already warns about, now for the shim itself. The NUMBER this section
# promised to assert instead of the query string is covered behaviourally: an aeon claim
# (event_type='claimed') by test-attempts-sql.sh's b4 ("three claims, never closed" = 3
# attempts) and a hand-driven in_progress transition (event_type='status_changed') by its
# sp-ev2 ("one in_progress transition is one attempt"); both event kinds are matched
# explicitly in spira-claim's own EventKind::of (events.rs), unit-tested by
# `cargo test -p spira-claim events::tests`.

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
# claimed <id> <n> — n claim events (`status_changed` to in_progress), seeded the way
# test-attempts-sql.sh seeds its fixture: by SQL, never by driving bd's status around the
# lifecycle machine (sp-hyo5e). What is under test is the count, not the CLI that once
# produced the rows.
claimed() {
    local id="$1" n="$2" i=0 uuid
    while [ "$i" -lt "$n" ]; do
        uuid="$(python3 -c 'import uuid; print(uuid.uuid4())')"
        bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', 'status_changed', 'harness', '{\"status\":\"in_progress\"}', NOW())" >/dev/null 2>&1
        i=$((i+1))
    done
}
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
# what stays here is the multi-event complement to that file's b3/b4 fixture, through the
# poison threshold: genuine failure still poisons via events, three in_progress events reach
# it.
poisons() { local n; n="$(num "$(attempts_of "$1")")"; [ "$n" -ge 3 ] && echo yes || echo no; }
seed sp-c2
claimed sp-c2 2
is "two events do not poison yet" no "$(poisons sp-c2)"
claimed sp-c2 1
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
echo "the release (real lifecycle store):"

# THE DISCRIMINATING FACT, seen both ways. The claim records the aeon's own name (BEADS_ACTOR
# =aeon-<instance>); the teardown, release_own_claim -> `spira-lc unclaim` (sp-hyo5e), must
# name that same actor or the machine's holder check can never match. The claim is the
# lifecycle row's — the one claim there is (sp-v62vn: bd's status and assignee are nobody's
# claim, and unclaim writes nothing there) — so this block stands up a real spira-lc store
# (testlib/lc-fixture.sh) and seeds the row WORKING under the aeon's name.
# shellcheck disable=SC1091
. "$HERE/testlib/lc-fixture.sh"
trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
lcfix_up || bail "lc-fixture: the lifecycle store did not come up"
lcfix_seed sp-r1 WORKING || bail "lc-fixture: could not seed sp-r1"
lcfix_sql -q "UPDATE bead SET holder='aeon-cindy', lease_until=$(( $(date +%s) + 3600 )) WHERE bead_id='sp-r1'" >/dev/null \
    || bail "lc-fixture: could not set sp-r1's holder"
lc_holder() { lcfix_sql -q "SELECT IFNULL(holder,'') FROM bead WHERE bead_id='$1'" -r csv 2>/dev/null | sed -n 2p; }
is "the fixture holds the bead under the aeon's name" "WORKING aeon-cindy" "$(lcfix_state sp-r1) $(lc_holder sp-r1)"

if BEADS_ACTOR=aeon-builder release_own_claim sp-r1; then r=0; else r=1; fi
is "releasing as the fayth fails"        1                    "$r"
is "and leaves the bead held"            "WORKING aeon-cindy" "$(lcfix_state sp-r1) $(lc_holder sp-r1)"

if BEADS_ACTOR=aeon-cindy release_own_claim sp-r1; then r=0; else r=1; fi
is "releasing as the aeon succeeds"      0       "$r"
is "and the bead is claimable again"     "READY" "$(lcfix_state sp-r1)"
tl_summary
