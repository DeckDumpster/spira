#!/usr/bin/env bash
#
# test-summon-fast-path.sh — sentinel.sh --summon-only (sp-0y2av): a free aeon slot must
# refill in seconds, not after a 4-5 minute full pass.
#
#   ./test-summon-fast-path.sh
#
# FOUR THINGS THIS PROVES, each with a positive control first:
#   A. aeon_count counts by unit name, not pidfile — the pidfile gap (aeon.sh writes its
#      pidfile only after it claims a bead) must not read a just-summoned aeon as free.
#   B. ck7_summon_pass's summon.lock actually serializes: the unlocked body races and
#      double-summons past its own cap; the locked wrapper does not.
#   C. ready-bucket.py's bucketing mirrors fayth_ready's predicate exactly (labels,
#      excludes, fayth: preference) — checked directly, no database needed.
#   D. sentinel.sh --summon-only, against a real fixture: ONE bd ready call (not one per
#      partition), a summon happens, and none of the full pass's own checks run.
#
# POSITIVE CONTROLS FIRST (law-a-regression-test-must-be-seen-to-fail).
#
# defect: sp-0y2av
# tier: T1
# covers: spira/lib.sh spira/sentinel.sh spira/ready-bucket.py UC-dispatch-09
# hermetic-ok: no real systemd, no real database in sections A-C; a fixture database in D
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'testdb_drop 2>/dev/null; rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run" "$T/chamber" "$T/bin"
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T"
export SPIRA_DB="$T/no-db"

. "$HERE/lib.sh"

echo "test-summon-fast-path.sh"

# ============================================================================
echo
echo "A — aeon_count counts live units by name, closing the pidfile gap:"
# ============================================================================
export MOCK_UNITS_FILE="$T/mock-units"   # read by mock-systemctl, a separate process
cat > "$T/bin/mock-systemctl" <<'MOCK'
#!/usr/bin/env bash
# list-units <glob> --no-legend -> lines from MOCK_UNITS_FILE matching <glob>, bash-glob style.
if [ "$2" = list-units ]; then
    glob="$3"
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        case "$line" in $glob) printf '%s\n' "$line" ;; esac
    done < "$MOCK_UNITS_FILE"
fi
MOCK
chmod +x "$T/bin/mock-systemctl"
export SPIRA_SYSTEMCTL="$T/bin/mock-systemctl"
unset SPIRA_SUMMON 2>/dev/null || true   # default is systemd-run -> aeon_count's unit path

# POSITIVE CONTROL: no unit, no pidfile -> 0.
: > "$MOCK_UNITS_FILE"
is "positive control: no unit at all -> aeon_count returns 0" "0" "$(aeon_count builder)"

# THE GAP ITSELF: a unit exists (systemd-run returned) but aeon.sh has not written its
# pidfile yet (it writes one only after claiming a bead) — the fast path must still see
# this aeon as live, or a second summon lands on a slot that is not actually free.
printf 'spira-aeon-builder-1700000000.service\n' > "$MOCK_UNITS_FILE"
rm -f "$SPIRA_RUN"/aeon-builder-*.pid
is "the pidfile gap: a live unit with NO pidfile still counts as 1" "1" "$(aeon_count builder)"

# PRECISION: a unit for a DIFFERENT fayth does not count toward this one.
is "precision: a builder unit does not count toward ops" "0" "$(aeon_count ops)"

# Two units for the same fayth -> 2.
printf 'spira-aeon-builder-1700000001.service\n' >> "$MOCK_UNITS_FILE"
is "two live units -> aeon_count returns 2" "2" "$(aeon_count builder)"

echo
echo "A (fallback) — a non-systemd-run SPIRA_SUMMON (test doubles) still counts by pidfile:"
export SPIRA_SUMMON="$T/bin/mock-summon-noop"
printf '#!/bin/sh\nexit 0\n' > "$SPIRA_SUMMON"; chmod +x "$SPIRA_SUMMON"
: > "$MOCK_UNITS_FILE"   # a live unit must be IGNORED in fallback mode
printf 'spira-aeon-builder-1700000002.service\n' > "$MOCK_UNITS_FILE"
rm -f "$SPIRA_RUN"/aeon-builder-*.pid
is "fallback: a unit is ignored when SPIRA_SUMMON is not systemd-run" "0" "$(aeon_count builder)"
# aeon_alive requires the pid's own cmdline to contain "aeon.sh" (never pgrep -f, so a
# fixture has to earn the match on /proc the same way a real aeon would): argv[0] set via
# `exec -a` to a name containing it, on a real backgrounded process.
( exec -a aeon.sh sleep 5 ) &
FAKE_AEON_PID=$!
printf '%s' "$FAKE_AEON_PID" > "$SPIRA_RUN/aeon-builder-sp-fallback.pid"
is "fallback: a live pidfile still counts (no real systemd needed)" "1" "$(aeon_count builder)"
kill "$FAKE_AEON_PID" 2>/dev/null; wait "$FAKE_AEON_PID" 2>/dev/null
rm -f "$SPIRA_RUN"/aeon-builder-*.pid
unset SPIRA_SUMMON 2>/dev/null || true

# ============================================================================
echo
echo "B — ck7_summon_pass's flock actually matters:"
# ============================================================================
cat > "$T/chamber/racer.fayth" <<'F'
FAYTH_NAME=racer
FAYTH_LABELS="test"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=60
F
export SPIRA_FAYTHS=racer
export SPIRA_MAX_AEONS=1
unset SPIRA_MAX_LIVE_AEONS SPIRA_LANES_MAX_LIVE 2>/dev/null || true

RACE_LOG="$T/race-summoned.log"
capacity_paused() { return 1; }
world_gate() { return 0; }
fayth_ready() { printf '1'; }
# A DELIBERATE, WIDE WINDOW: the real defect is a race between two processes reading
# "N live" before either's summon lands (aeon.sh writes its pidfile late; a second
# process's systemd-run call takes real wall-clock time). 0.3s is generous enough that
# two backgrounded shells started within milliseconds of each other reliably overlap
# inside it, without depending on true OS scheduling nondeterminism.
aeon_count() { sleep 0.3; local n; n="$(grep -c . "$RACE_LOG" 2>/dev/null)"; printf '%s' "${n:-0}"; }
cat > "$T/bin/mock-summon-race" <<EOF
#!/usr/bin/env bash
echo summoned >> "$RACE_LOG"
EOF
chmod +x "$T/bin/mock-summon-race"
export SPIRA_SUMMON="$T/bin/mock-summon-race"
acted=0; act() { acted=$((acted+1)); }

echo
echo "B negative control — the UNLOCKED body races and double-summons past cap=1:"
: > "$RACE_LOG"; rm -f "$SPIRA_RUN/summon.lock"
( _ck7_summon_body ) & ( _ck7_summon_body ) &
wait
is "unlocked: two concurrent bodies both summon (cap=1 exceeded — the race is real)" \
   "2" "$(grep -c . "$RACE_LOG" 2>/dev/null || echo 0)"

echo
echo "B — the LOCKED wrapper serializes the same race and holds the cap:"
: > "$RACE_LOG"; rm -f "$SPIRA_RUN/summon.lock"
( ck7_summon_pass ) & ( ck7_summon_pass ) &
wait
is "locked: two concurrent passes summon exactly once (cap=1 honoured)" \
   "1" "$(grep -c . "$RACE_LOG" 2>/dev/null || echo 0)"

unset -f capacity_paused world_gate fayth_ready aeon_count act
unset SPIRA_SUMMON SPIRA_FAYTHS SPIRA_MAX_AEONS acted 2>/dev/null || true

# ============================================================================
echo
echo "C — ready-bucket.py mirrors fayth_ready's own predicate:"
# ============================================================================
RB="$HERE/ready-bucket.py"
[ -r "$RB" ] || bail "ready-bucket.py not readable at $RB"

PARTS_FIXTURE="builder|plan|spira-poison
ops|incident|spira-poison"

run_bucket() {   # run_bucket <beads-json>
    printf '%s' "$1" | PARTS="$PARTS_FIXTURE" python3 "$RB"
}

# POSITIVE CONTROL: a plain bead matching builder's labels counts for builder, not ops.
out="$(run_bucket '[{"id":"sp-1","labels":["plan"]}]')"
want "positive control: plain plan bead counts for builder" "builder 1" "$out"
want "positive control: plain plan bead does not count for ops" "ops 0" "$out"

# EXCLUDE LABEL: a bead carrying the fayth's own exclude label does not count for it.
out="$(run_bucket '[{"id":"sp-2","labels":["plan","spira-poison"]}]')"
is "exclude label: a poisoned plan bead counts for nobody" "builder 0
ops 0" "$out"

# FAYTH: PREFERENCE NARROWS. A bead naming fayth:ops is excluded from builder even though
# it also carries builder's own labels.
out="$(run_bucket '[{"id":"sp-3","labels":["plan","incident","fayth:ops"]}]')"
want "fayth: preference: named persona still counts" "ops 1" "$out"
want "fayth: preference: every other persona is excluded" "builder 0" "$out"

# SHARED EXCLUDE (QUEUE_WAIT/SUBMITTED): excluded from every fayth regardless of labels.
out="$(printf '%s' '[{"id":"sp-4","labels":["plan","queue-wait"]}]' \
    | PARTS="$PARTS_FIXTURE" SPIRA_QUEUE_WAIT_LABEL=queue-wait python3 "$RB")"
is "shared exclude: a queue-wait bead counts for nobody" "builder 0
ops 0" "$out"

# COUNTS ACCUMULATE across multiple beads in one call — the whole point of ONE bd call.
out="$(run_bucket '[{"id":"sp-5","labels":["plan"]},{"id":"sp-6","labels":["plan"]},{"id":"sp-7","labels":["incident"]}]')"
is "counts accumulate: 2 builder beads, 1 ops bead in a single fetch" "builder 2
ops 1" "$out"

# ============================================================================
echo
echo "D — sentinel.sh --summon-only against a real fixture: one bd ready call, no full pass:"
# ============================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-summon-fast-path
testdb_up summon_fast || { echo "test-summon-fast-path: could not build fixture"; exit 1; }
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-goal1","title":"goal","status":"open","issue_type":"epic","labels":["plan"]}
{"id":"sp-b1","title":"bead 1","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b2","title":"bead 2","status":"open","issue_type":"task","labels":["plan"]}
JSONL

DSTUBS="$T/dstubs"
mkdir -p "$DSTUBS"
ln -s "$HERE/chamber" "$DSTUBS/chamber"
ln -s "$HERE/ready-bucket.py" "$DSTUBS/ready-bucket.py"
SUMMON_LOG="$T/d-summoned.log"
printf '#!/bin/sh\necho summoned >> "%s"\n' "$SUMMON_LOG" > "$DSTUBS/mock-summon"; chmod +x "$DSTUBS/mock-summon"
# SPIRA_SUMMON below is this mock, not systemd-run, so aeon_count takes its pidfile
# fallback path (section A's fallback control) and never asks systemctl at all here.

BD_CALL_LOG="$T/bd-calls.log"
# REAL_BD is resolved to the ACTUAL binary testdb_up exported, captured now — before
# run_summon_only overrides SPIRA_BD to this wrapper for the subprocess. Embedding the
# resolved value (not another read of $SPIRA_BD) is what keeps this a counting passthrough
# to the real dependency rather than a self-recursive stub.
REAL_BD="${SPIRA_BD:-bd}"
cat > "$DSTUBS/counting-bd" <<EOF
#!/usr/bin/env bash
echo "\$*" >> "$BD_CALL_LOG"
exec "$REAL_BD" "\$@"
EOF
chmod +x "$DSTUBS/counting-bd"

run_summon_only() {   # run_summon_only <run-dir> [KEY=VAL ...]
    local run="$1"; shift
    mkdir -p "$run"
    env -i \
        PATH="$PATH" HOME="$HOME" \
        SPIRA_HOME="$DSTUBS" \
        SPIRA_RUN="$run" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$DSTUBS/counting-bd" \
        SPIRA_GOAL="sp-goal1" \
        SPIRA_SUMMON="$DSTUBS/mock-summon" \
        SPIRA_FAYTHS=builder SPIRA_SCOPE_LABEL= SPIRA_MAX_AEONS=2 \
        "$@" \
        bash "$HERE/sentinel.sh" --summon-only 2>&1
}

# builder is elastic (FAYTH_ELASTIC=1, builder.fayth): its fill loop is bound by the POOL,
# not by the ready count, because the mock summon below never actually claims a bead — the
# same shape test-sentinel-pass.sh's own "fill" pass exercises. Pool=2 here so the summon
# count this asserts (2) is unambiguous rather than an artifact of the fixture's 2 ready beads.
#
# ONE SHARED $SPIRA_RUN FOR THE WHOLE SECTION, PRIMED FIRST. conf.sh runs its own `bd
# migrate schema` the first time any script sources it against a given $SPIRA_RUN (cached
# by a stamp file there after) — orthogonal to sentinel.sh's own bd usage, but a bd call
# all the same, and counted by the same wrapper. Priming it once, behind world.halted so
# nothing is summoned by the prime itself, keeps that unrelated check out of the counts
# below as far as it can; the assertions themselves count "ready" calls specifically
# (never a bare call count) so a leftover connection retry cannot flip them either.
_drun="$T/run-summon-only"; mkdir -p "$_drun"
: > "$_drun/world.halted"
run_summon_only "$_drun" >/dev/null 2>&1
rm -f "$_drun/world.halted"

rm -f "$SUMMON_LOG" "$BD_CALL_LOG"
out_d1="$(run_summon_only "$_drun")"
is "D: pool=2 -> exactly 2 summons (elastic fill, bounded by the pool)" "2" "$(grep -c . "$SUMMON_LOG" 2>/dev/null || echo 0)"
is "D: exactly ONE bd call fetched the ready set" "1" "$(grep -c ' ready ' "$BD_CALL_LOG" 2>/dev/null || echo 0)"
want "D: the one call was a ready query" "ready" "$(cat "$BD_CALL_LOG" 2>/dev/null)"
want "D: log reports the summon-only pass" "summon-only pass complete" "$out_d1"
nowant "D: no full-pass state line" "state: goal=" "$out_d1"
nowant "D: no full-pass CHECK7c" "CHECK7c" "$out_d1"
nowant "D: no Sending" "sending:" "$out_d1"

echo
echo "D — world.halted short-circuits before the live count or any bd call:"
rm -f "$SUMMON_LOG" "$BD_CALL_LOG"
: > "$_drun/world.halted"
out_halted="$(run_summon_only "$_drun")"
is "halted: no summon happens" "0" "$(grep -c . "$SUMMON_LOG" 2>/dev/null || echo 0)"
is "halted: no bd 'ready' call is made" "0" "$(grep -c ' ready ' "$BD_CALL_LOG" 2>/dev/null || echo 0)"
want "halted: says so" "halted — not summoning" "$out_halted"

# ============================================================================
echo
echo "E — every aeon carries its own fast-path refill hook (ExecStopPost):"
# ============================================================================
# A summon a full pass or --summon-only both start (the aeon unit itself needs no timer to
# know it exited) — the refill it wires must reach --summon-only, never the full pass, or
# an aeon exiting is right back to waiting out the 2-minute cadence this bead exists to cut.
export SPIRA_SUMMON="$T/bin/mock-summon-noop"   # already an absolute path; created in section A
argv="$(summon_argv racer | tr '\n' ' ')"
case "$argv" in
    *"--property=ExecStopPost=$T/bin/mock-summon-noop --user --collect --quiet $T/sentinel.sh --summon-only"*)
        ok "summon_argv: ExecStopPost refills via --summon-only, not a full pass" ;;
    *) bad "summon_argv: ExecStopPost refills via --summon-only" "got: $argv" ;;
esac
unset SPIRA_SUMMON 2>/dev/null || true

tl_summary
