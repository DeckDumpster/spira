#!/usr/bin/env bash
#
# test-summon-fast-path.sh — sentinel.sh --summon-only (sp-0y2av): a free aeon slot must
# refill in seconds, not after a 4-5 minute full pass.
#
#   ./test-summon-fast-path.sh
#
# THREE THINGS THIS PROVES, each with a positive control first:
#   A. aeon_count counts by unit name, not pidfile — the pidfile gap (aeon.sh writes its
#      pidfile only after it claims a bead) must not read a just-summoned aeon as free.
#   C. ready-bucket.py's bucketing mirrors fayth_ready's predicate exactly (labels,
#      excludes, fayth: preference) — checked directly, no database needed.
#   D. sentinel --summon-only, against a real fixture: the ready set is spira-claim's, over
#      the lifecycle machine's READY rows — ZERO bd ready calls (sp-v62vn: there is no off
#      mode, so neither the sentinel nor spira-claim asks bd what is ready) — a summon
#      happens, beads bd calls open but the machine does not call READY summon nothing, and
#      none of the full pass's own checks run.
#
# B — ck7_summon_pass's summon.lock serializing two real contenders — MOVED (wave 4.27,
#     family G, sp-gzmd2): `_ck7_summon_body`, bare and unlocked, used to be reachable as
#     a bash function this suite could stub (aeon_count/capacity_paused/world_gate/
#     fayth_ready/act, by name) to PROVE the unlocked race and then prove the lock
#     removes it. `ck7_summon_pass`/`world_gate`/`summon_fayth` are now one-line shims
#     onto a compiled `sentinel` process, which offers no shell-function-by-name override
#     — the race this row used to demonstrate is now structurally impossible rather than
#     merely refused: there is no entry point to the unlocked body left to call bare. The
#     lock itself (the exact `libc::flock` primitive `acquire_summon_lock` takes, proven
#     against two real OS threads racing to enter) is
#     `summon::tests::the_lock_actually_serializes_two_real_contenders` in
#     sentinel/src/summon.rs; what the lock protects — two real `ck7_summon_pass` calls in
#     sequence sharing the fleet correctly — is
#     `sentinel::tests::ck7_summon_pass_rotates_across_two_real_passes` in
#     sentinel/src/tests.rs.
#
# POSITIVE CONTROLS FIRST (law-a-regression-test-must-be-seen-to-fail).
#
# defect: sp-0y2av
# tier: T1
# covers: spira/lib.sh sentinel/src/* spira/ready-bucket.py UC-dispatch-09
# hermetic-ok: no real systemd, no real database in sections A-C; a fixture database in D
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"
_fastpath_cleanup() { testdb_drop 2>/dev/null; rm -rf "$T"; }
# sp-pnogc (round 209, check 29): EXIT alone runs the handler and stops, which is fine, but
# bash's default for a TRAPPED INT/TERM is to run the handler and then CONTINUE the script
# — the signal is "handled", not fatal, unless the handler itself exits. The original
# 'EXIT INT TERM' trap here only ever cleaned up; under a full-corpus sweep this suite can
# outlive its share of testenv's per-suite wall budget and be sent SIGTERM mid-run (DESIGN
# D7/D9) — the handler fired, deleted $T, and the script kept going into section E, whose
# sentinel probe then found $T/lib.sh gone ("No such file or directory", rc=97) and recorded
# a false, misleading red instead of the real timeout. Re-raising after cleanup restores the
# signal's own default disposition so a genuinely killed suite stays killed.
trap _fastpath_cleanup EXIT
trap '_fastpath_cleanup; trap - INT; kill -INT $$' INT
trap '_fastpath_cleanup; trap - TERM; kill -TERM $$' TERM
mkdir -p "$T/run" "$T/chamber" "$T/bin"
# lib.sh sources conf.sh, which resolves every registered key straight from SPIRA_TOML
# (inherited here — this is the main suite shell, not under env -i), so these are declared
# through tl_config rather than export, or conf.sh's own resolve would overwrite them right
# back to the fixture's values the moment lib.sh is sourced below.
# SPIRA_CHAMBER too — the fixture declares a fixed, nonexistent path; nothing derives it
# from SPIRA_HOME any more (sfail round 2, pattern 6).
tl_config SPIRA_RUN="$T/run" SPIRA_DB="$T/no-db" SPIRA_CHAMBER="$T/chamber"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T" PATH="$T:$PATH"

# `summon_argv` (section E) is a `sentinel --summon-argv` shim now (wave 4.27, family G,
# sp-gzmd2) — a real subprocess with SPIRA_HOME=$T, which needs a working $T/lib.sh to
# source at its own context probe. Same one-line symlink trick test-summon-fayth.sh's own
# `aeon --escape` fixture uses.
printf '. "%s/lib.sh"\n' "$HERE" > "$T/lib.sh"
ln -s "$HERE/conf.d" "$T/conf.d"

# THE AEON IS A BINARY (aeon.sh is gone): summon_fayth hands systemd-run the aeon it finds
# on PATH (sp-gypjk). The mock SPIRA_SUMMON never execs it.
command -v aeon >/dev/null 2>&1 \
    || { echo "test-summon-fast-path: aeon is not on PATH" >&2; exit 1; }

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

# SELF-EXCLUSION (sp-0hnm6): a caller's own unit must not count toward its own capacity
# question, or the FIRST aeon of a fayth with max=1 reads "1/1 at capacity" forever.
: > "$MOCK_UNITS_FILE"
printf 'spira-aeon-builder-1700000002.service\n' > "$MOCK_UNITS_FILE"
is "sole unit is the caller's own -> excluded, count is 0" "0" \
   "$(aeon_count builder spira-aeon-builder-1700000002.service)"
printf 'spira-aeon-builder-1700000003.service\n' >> "$MOCK_UNITS_FILE"
is "own unit excluded, one OTHER unit still counts -> 1" "1" \
   "$(aeon_count builder spira-aeon-builder-1700000002.service)"
is "no exclusion given -> both units count (unaffected callers keep old behaviour)" "2" \
   "$(aeon_count builder)"

# fayth_free must carry the same exclusion through to aeon_count — aeon.sh's own
# capacity gate (aeon.sh:228) goes through fayth_free, not aeon_count directly.
cat > "$T/chamber/onecap.fayth" <<'F'
FAYTH_NAME=onecap
FAYTH_LABELS="test"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=60
F
: > "$MOCK_UNITS_FILE"
printf 'spira-aeon-onecap-self.service\n' > "$MOCK_UNITS_FILE"
is "fayth_free: sole unit is caller's own -> 1 slot free, not 0" "1" \
   "$(fayth_free onecap "" spira-aeon-onecap-self.service)"
printf 'spira-aeon-onecap-other.service\n' >> "$MOCK_UNITS_FILE"
is "fayth_free: own unit excluded, one other live -> 0 free (still capped)" "0" \
   "$(fayth_free onecap "" spira-aeon-onecap-self.service)"
rm -f "$T/chamber/onecap.fayth"

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
# Kill-on-exit (sp-r70dc): fold into cleanup as a backstop, so a failure between here and
# the explicit kill below cannot leave this running for its full 5s unkilled. Redefining
# the function is enough — the trap commands above already call it by name on EXIT/INT/TERM.
_fastpath_cleanup() { kill "$FAKE_AEON_PID" 2>/dev/null; testdb_drop 2>/dev/null; rm -rf "$T"; }
# sp-pnogc (round 209, check 11): `$!` is valid the instant the subshell forks, but the
# `exec -a aeon.sh sleep 5` inside it has not necessarily landed by then — /proc/<pid>/cmdline
# still reads the pre-exec subshell's own argv for a few scheduler ticks, and aeon_alive
# (strand/src/probe.rs) requires the cmdline to actually say "aeon.sh". Wait on that real
# condition rather than assuming the race already lost — never a fixed sleep.
_wait_execed_as() {   # _wait_execed_as <pid> <needle> -> 0 once /proc/<pid>/cmdline contains it, 1 if the pid dies first
    local _pid="$1" _want="$2"
    while [ -d "/proc/$_pid" ]; do
        case "$(tr '\0' ' ' < "/proc/$_pid/cmdline" 2>/dev/null)" in
            *"$_want"*) return 0 ;;
        esac
    done
    return 1
}
_wait_execed_as "$FAKE_AEON_PID" "aeon.sh" || bail "fixture never exec'd into aeon.sh"
printf '%s' "$FAKE_AEON_PID" > "$SPIRA_RUN/aeon-builder-sp-fallback.pid"
is "fallback: a live pidfile still counts (no real systemd needed)" "1" "$(aeon_count builder)"
kill "$FAKE_AEON_PID" 2>/dev/null; wait "$FAKE_AEON_PID" 2>/dev/null
rm -f "$SPIRA_RUN"/aeon-builder-*.pid
unset SPIRA_SUMMON 2>/dev/null || true

# Section B (the summon.lock race/serialization proof) moved — see the header note above.

# ============================================================================
echo
echo "C — ready-bucket.py mirrors fayth_ready's own predicate:"
# ============================================================================
RB="$HERE/ready-bucket.py"
[ -r "$RB" ] || bail "ready-bucket.py not readable at $RB"

PARTS_FIXTURE="builder|plan|spira-poison
ops|incident|spira-poison"

run_bucket() {   # run_bucket <beads-json>
    printf '%s' "$1" | PARTS="$PARTS_FIXTURE" ready-bucket.py
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
    | PARTS="$PARTS_FIXTURE" SPIRA_QUEUE_WAIT_LABEL=queue-wait ready-bucket.py)"
is "shared exclude: a queue-wait bead counts for nobody" "builder 0
ops 0" "$out"

# COUNTS ACCUMULATE across multiple beads in one call — the whole point of ONE bd call.
out="$(run_bucket '[{"id":"sp-5","labels":["plan"]},{"id":"sp-6","labels":["plan"]},{"id":"sp-7","labels":["incident"]}]')"
is "counts accumulate: 2 builder beads, 1 ops bead in a single fetch" "builder 2
ops 1" "$out"

# ============================================================================
echo
echo "D — sentinel --summon-only against a real fixture: the machine's ready set, no bd ready call, no full pass:"
# ============================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-summon-fast-path
testdb_up summon_fast || { echo "test-summon-fast-path: could not build fixture"; exit 1; }
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-epic1","title":"epic","status":"open","issue_type":"epic","labels":["plan"]}
{"id":"sp-b1","title":"bead 1","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b2","title":"bead 2","status":"open","issue_type":"task","labels":["plan"]}
JSONL

DSTUBS="$T/dstubs"
mkdir -p "$DSTUBS"
ln -s "$HERE/chamber" "$DSTUBS/chamber"
ln -s "$HERE/ready-bucket.py" "$DSTUBS/ready-bucket.py"
# THE SENTINEL IS A BINARY (sentinel.sh is gone): it sources lib.sh from SPIRA_HOME.
for _s in lib.sh conf.sh suite-covers.sh; do ln -s "$HERE/$_s" "$DSTUBS/$_s"; done
ln -s "$HERE/conf.d" "$DSTUBS/conf.d"
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

# THE READY SET IS THE MACHINE'S. spira-claim (bulk-ready-by-fayth, inside the summon pass)
# reads the lifecycle machine's READY rows from `spira-lc list`, then bd only for those
# beads' content. The stand-in (testlib lc_mirror_bd) answers `list` from the fixture's
# real bd store — open beads are READY rows, unless $LCMIRROR/states pins a row otherwise —
# and a counting wrapper ahead of it on PATH (spira-claim finds spira-lc by name) records
# that the machine was asked at all.
LCMIRROR="$T/lcmirror"; LCBIN="$T/lcbin"; LC_CALL_LOG="$T/lc-calls.log"
lc_mirror_bd "$LCMIRROR"
mkdir -p "$LCBIN"
cat > "$LCBIN/spira-lc" <<EOF
#!/usr/bin/env bash
echo "\$*" >> "$LC_CALL_LOG"
exec "$LCMIRROR/spira-lc" "\$@"
EOF
chmod +x "$LCBIN/spira-lc"

run_summon_only() {   # run_summon_only <run-dir> [KEY=VAL ...]
    local run="$1"; shift
    mkdir -p "$run"
    tl_config SPIRA_RUN="$run" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$DSTUBS/counting-bd" \
        SPIRA_FAYTHS=builder SPIRA_SCOPE_LABEL="" SPIRA_MAX_AEONS=2 \
        SPIRA_CHAMBER="$DSTUBS/chamber"
    # SPIRA_DB/SPIRA_BD also passed literally: the spira-lc stub chain (lc_mirror_bd,
    # wrapped by LCBIN's counting shim) is plain bash reading them straight from its own
    # environment, never through spira-config (sfail round 3, pattern 8).
    env -i \
        PATH="$LCBIN:$DSTUBS:$PATH" HOME="$HOME" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$DSTUBS" \
        SPIRA_SUMMON="$DSTUBS/mock-summon" \
        SPIRA_DB="$SPIRA_DB" SPIRA_BD="$DSTUBS/counting-bd" \
        "$@" \
        sentinel --summon-only 2>&1
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

rm -f "$SUMMON_LOG" "$BD_CALL_LOG" "$LC_CALL_LOG"
out_d1="$(run_summon_only "$_drun")"
is "D: pool=2 -> exactly 2 summons (elastic fill, bounded by the pool)" "2" "$(grep -c . "$SUMMON_LOG" 2>/dev/null || echo 0)"
want "D: the ready set was read from the lifecycle machine (spira-lc list)" "list" "$(cat "$LC_CALL_LOG" 2>/dev/null)"
# POSITIVE CONTROL for the zero below: the counting wrapper does see this pass's bd reads
# (spira-claim's content read of the READY beads), so a zero is not a dead log.
want "D: positive control: the pass's bd reads reach the counting wrapper" "list" "$(cat "$BD_CALL_LOG" 2>/dev/null)"
# grep -c prints 0 AND exits 1 on no match, so no `|| echo 0` here (that printed "0\n0"); the
# positive control above already proved the log exists.
is "D: ZERO bd ready calls — the summon pass's ready set is the machine's" "0" "$(grep -c ' ready ' "$BD_CALL_LOG" 2>/dev/null)"
want "D: log reports the summon-only pass" "summon-only pass complete" "$out_d1"
nowant "D: no full-pass state line" "state: open=" "$out_d1"
nowant "D: no full-pass CHECK7c" "CHECK7c" "$out_d1"
nowant "D: no Sending" "sending:" "$out_d1"

echo
echo "D — the machine, not bd, decides: beads bd calls open but the machine holds summon nothing:"
# Same store, same pool; the machine now says both beads are WORKING. bd still calls them
# open — a summoner reading bd's status would fill the pool again.
printf 'sp-b1 WORKING\nsp-b2 WORKING\n' > "$LCMIRROR/states"
rm -f "$SUMMON_LOG" "$BD_CALL_LOG"
out_held="$(run_summon_only "$_drun")"
is "held: no summon when the machine has no READY row" "0" "$(grep -c . "$SUMMON_LOG" 2>/dev/null || echo 0)"
want "held: says nothing is ready" "nothing ready in its partition" "$out_held"
rm -f "$LCMIRROR/states"

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
# The refill runs the sentinel BINARY (sentinel.sh is gone), resolved on PATH (sp-gypjk):
# ExecStopPost needs an absolute path, so summon_argv hands it its own resolved path.
# `summon_argv` is itself a `sentinel --summon-argv` shim now (wave 4.27, family G,
# sp-gzmd2) — a real execution, not a pure string computation in this shell — so it
# necessarily resolves to WHATEVER "sentinel" is really on PATH, not a stand-in; the
# assertion below resolves the same name through the same PATH rather than asserting a
# fixed fake path no real binary would ever embed as itself.
sentinel_path="$(PATH="$T/bin:$PATH" command -v sentinel)"
argv="$(PATH="$T/bin:$PATH" summon_argv racer | tr '\n' ' ')"
case "$argv" in
    *"--property=ExecStopPost=$T/bin/mock-summon-noop --user --collect --quiet "*"--setenv=SPIRA_TOML="*" $sentinel_path --summon-only"*)
        ok "summon_argv: ExecStopPost refills via --summon-only, not a full pass, with SPIRA_TOML" ;;
    *) bad "summon_argv: ExecStopPost refills via --summon-only" "got: $argv (sentinel resolved to $sentinel_path)" ;;
esac
unset SPIRA_SUMMON 2>/dev/null || true

tl_summary
