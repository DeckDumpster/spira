#!/usr/bin/env bash
#
# test-call-fast-path.sh — sentinel.sh --call-only (sp-0y2av): a free dwarf slot must
# refill in seconds, not after a 4-5 minute full pass.
#
#   ./test-call-fast-path.sh
#
# THREE THINGS THIS PROVES, each with a positive control first:
#   A. dwarf_count counts by unit name, not pidfile — the pidfile gap (dwarf.sh writes its
#      pidfile only after it claims a bead) must not read a just-called dwarf as free.
#   C. (the bucketing predicate is spira-claim's, unit-tested in spira-claim/src/ready.rs.)
#   D. sentinel --call-only, against a real fixture: the ready set is spira-claim's, over
#      the lifecycle machine's READY rows — ZERO bd ready calls (sp-v62vn: there is no off
#      mode, so neither the sentinel nor spira-claim asks bd what is ready) — a call
#      happens, beads bd calls open but the machine does not call READY call nothing, and
#      none of the full pass's own checks run.
#
# B — ck7_call_pass's call.lock serializing two real contenders — MOVED (wave 4.27,
#     family G, sp-gzmd2): `_ck7_call_body`, bare and unlocked, used to be reachable as
#     a bash function this suite could stub (dwarf_count/capacity_paused/world_gate/
#     rune_ready/act, by name) to PROVE the unlocked race and then prove the lock
#     removes it. `ck7_call_pass`/`world_gate`/`call_rune` are now one-line shims
#     onto a compiled `sentinel` process, which offers no shell-function-by-name override
#     — the race this row used to demonstrate is now structurally impossible rather than
#     merely refused: there is no entry point to the unlocked body left to call bare. The
#     lock itself (the exact `libc::flock` primitive `acquire_call_lock` takes, proven
#     against two real OS threads racing to enter) is
#     `call::tests::the_lock_actually_serializes_two_real_contenders` in
#     sentinel/src/call.rs; what the lock protects — two real `ck7_call_pass` calls in
#     sequence sharing the fleet correctly — is
#     `sentinel::tests::ck7_call_pass_rotates_across_two_real_passes` in
#     sentinel/src/tests.rs.
#
# POSITIVE CONTROLS FIRST (law-a-regression-test-must-be-seen-to-fail).
#
# defect: sp-0y2av
# tier: T1
# covers: spira/lib.sh sentinel/src/* UC-dispatch-09
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
mkdir -p "$T/run" "$T/runes" "$T/bin"
# lib.sh sources conf.sh, which resolves every registered key straight from SPIRA_TOML
# (inherited here — this is the main suite shell, not under env -i), so these are declared
# through tl_config rather than export, or conf.sh's own resolve would overwrite them right
# back to the fixture's values the moment lib.sh is sourced below.
# SPIRA_RUNES_DIR too — the fixture declares a fixed, nonexistent path; nothing derives it
# from SPIRA_HOME any more (sfail round 2, pattern 6).
tl_config SPIRA_RUN="$T/run" SPIRA_DB="$T/no-db" SPIRA_RUNES_DIR="$T/runes"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T" PATH="$T:$PATH"

# `call_argv` (section E) is a `sentinel --call-argv` shim now (wave 4.27, family G,
# sp-gzmd2) — a real subprocess with SPIRA_HOME=$T, which needs a working $T/lib.sh to
# source at its own context probe. Same one-line symlink trick test-call-rune.sh's own
# `dwarf --escape` fixture uses.
printf '. "%s/lib.sh"\n' "$HERE" > "$T/lib.sh"
ln -s "$HERE/conf.d" "$T/conf.d"
ln -s "$HERE/conf.toml.d" "$T/conf.toml.d"

# THE DWARF IS A BINARY (dwarf.sh is gone): call_rune hands systemd-run the dwarf it finds
# on PATH (sp-gypjk). The mock SPIRA_CALL never execs it.
command -v dwarf >/dev/null 2>&1 \
    || { echo "test-call-fast-path: dwarf is not on PATH" >&2; exit 1; }

. "$HERE/lib.sh"

echo "test-call-fast-path.sh"

# ============================================================================
echo
echo "A — dwarf_count counts live units by name, closing the pidfile gap:"
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
unset SPIRA_CALL 2>/dev/null || true   # default is systemd-run -> dwarf_count's unit path

# POSITIVE CONTROL: no unit, no pidfile -> 0.
: > "$MOCK_UNITS_FILE"
is "positive control: no unit at all -> dwarf_count returns 0" "0" "$(dwarf_count builder)"

# THE GAP ITSELF: a unit exists (systemd-run returned) but dwarf.sh has not written its
# pidfile yet (it writes one only after claiming a bead) — the fast path must still see
# this dwarf as live, or a second call lands on a slot that is not actually free.
printf 'spira-dwarf-builder-1700000000.service\n' > "$MOCK_UNITS_FILE"
rm -f "$SPIRA_RUN"/dwarf-builder-*.pid "$SPIRA_RUN"/dwarf-builder-*.lease
is "the pidfile gap: a live unit with NO pidfile still counts as 1" "1" "$(dwarf_count builder)"

# PRECISION: a unit for a DIFFERENT rune does not count toward this one.
is "precision: a builder unit does not count toward ops" "0" "$(dwarf_count ops)"

# Two units for the same rune -> 2.
printf 'spira-dwarf-builder-1700000001.service\n' >> "$MOCK_UNITS_FILE"
is "two live units -> dwarf_count returns 2" "2" "$(dwarf_count builder)"

# SELF-EXCLUSION (sp-0hnm6): a caller's own unit must not count toward its own capacity
# question, or the FIRST dwarf of a rune with max=1 reads "1/1 at capacity" forever.
: > "$MOCK_UNITS_FILE"
printf 'spira-dwarf-builder-1700000002.service\n' > "$MOCK_UNITS_FILE"
is "sole unit is the caller's own -> excluded, count is 0" "0" \
   "$(dwarf_count builder spira-dwarf-builder-1700000002.service)"
printf 'spira-dwarf-builder-1700000003.service\n' >> "$MOCK_UNITS_FILE"
is "own unit excluded, one OTHER unit still counts -> 1" "1" \
   "$(dwarf_count builder spira-dwarf-builder-1700000002.service)"
is "no exclusion given -> both units count (unaffected callers keep old behaviour)" "2" \
   "$(dwarf_count builder)"

# rune_free must carry the same exclusion through to dwarf_count — dwarf.sh's own
# capacity gate (dwarf.sh:228) goes through rune_free, not dwarf_count directly.
cat > "$T/runes/onecap.rune" <<'F'
RUNE_NAME=onecap
RUNE_LABELS="test"
RUNE_MAX_CONCURRENT=1
RUNE_HEARTBEAT_SECONDS=60
F
: > "$MOCK_UNITS_FILE"
printf 'spira-dwarf-onecap-self.service\n' > "$MOCK_UNITS_FILE"
is "rune_free: sole unit is caller's own -> 1 slot free, not 0" "1" \
   "$(rune_free onecap "" spira-dwarf-onecap-self.service)"
printf 'spira-dwarf-onecap-other.service\n' >> "$MOCK_UNITS_FILE"
is "rune_free: own unit excluded, one other live -> 0 free (still capped)" "0" \
   "$(rune_free onecap "" spira-dwarf-onecap-self.service)"
rm -f "$T/runes/onecap.rune"

echo
echo "A (fallback) — a non-systemd-run SPIRA_CALL (test doubles) still counts by pidfile:"
export SPIRA_CALL="$T/bin/mock-call-noop"
printf '#!/bin/sh\nexit 0\n' > "$SPIRA_CALL"; chmod +x "$SPIRA_CALL"
: > "$MOCK_UNITS_FILE"   # a live unit must be IGNORED in fallback mode
printf 'spira-dwarf-builder-1700000002.service\n' > "$MOCK_UNITS_FILE"
rm -f "$SPIRA_RUN"/dwarf-builder-*.pid
is "fallback: a unit is ignored when SPIRA_CALL is not systemd-run" "0" "$(dwarf_count builder)"
# dwarf_alive requires the pid's own cmdline to contain "dwarf.sh" (never pgrep -f, so a
# fixture has to earn the match on /proc the same way a real dwarf would): argv[0] set via
# `exec -a` to a name containing it, on a real backgrounded process.
( exec -a dwarf.sh sleep 5 ) &
FAKE_DWARF_PID=$!
# Kill-on-exit (sp-r70dc): fold into cleanup as a backstop, so a failure between here and
# the explicit kill below cannot leave this running for its full 5s unkilled. Redefining
# the function is enough — the trap commands above already call it by name on EXIT/INT/TERM.
_fastpath_cleanup() { kill "$FAKE_DWARF_PID" 2>/dev/null; testdb_drop 2>/dev/null; rm -rf "$T"; }
# sp-pnogc (round 209, check 11): `$!` is valid the instant the subshell forks, but the
# `exec -a dwarf.sh sleep 5` inside it has not necessarily landed by then — /proc/<pid>/cmdline
# still reads the pre-exec subshell's own argv for a few scheduler ticks, and dwarf_alive
# (strand/src/probe.rs) requires the cmdline to actually say "dwarf.sh". Wait on that real
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
_wait_execed_as "$FAKE_DWARF_PID" "dwarf.sh" || bail "fixture never exec'd into dwarf.sh"
printf '%s' "$FAKE_DWARF_PID" > "$SPIRA_RUN/dwarf-builder-sp-fallback.pid"
printf '%s' "$(( $(date +%s) + 3600 ))" > "$SPIRA_RUN/dwarf-builder-sp-fallback.lease"
is "fallback: a live pidfile still counts (no real systemd needed)" "1" "$(dwarf_count builder)"
kill "$FAKE_DWARF_PID" 2>/dev/null; wait "$FAKE_DWARF_PID" 2>/dev/null
rm -f "$SPIRA_RUN"/dwarf-builder-*.pid
unset SPIRA_CALL 2>/dev/null || true

# Section B (the call.lock race/serialization proof) moved — see the header note above.

# ============================================================================
echo
echo "D — sentinel --call-only against a real fixture: the machine's ready set, no bd ready call, no full pass:"
# ============================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-call-fast-path
testdb_up call_fast || { echo "test-call-fast-path: could not build fixture"; exit 1; }
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-epic1","title":"epic","status":"open","issue_type":"epic","labels":["plan"]}
{"id":"sp-b1","title":"bead 1","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b2","title":"bead 2","status":"open","issue_type":"task","labels":["plan"]}
JSONL

DSTUBS="$T/dstubs"
mkdir -p "$DSTUBS"
ln -s "$HERE/runes" "$DSTUBS/runes"
# THE SENTINEL IS A BINARY (sentinel.sh is gone): it sources lib.sh from SPIRA_HOME.
for _s in lib.sh conf.sh suite-covers.sh; do ln -s "$HERE/$_s" "$DSTUBS/$_s"; done
ln -s "$HERE/conf.d" "$DSTUBS/conf.d"
ln -s "$HERE/conf.toml.d" "$DSTUBS/conf.toml.d"
CALL_LOG="$T/d-called.log"
printf '#!/bin/sh\necho called >> "%s"\n' "$CALL_LOG" > "$DSTUBS/mock-call"; chmod +x "$DSTUBS/mock-call"
# SPIRA_CALL below is this mock, not systemd-run, so dwarf_count takes its pidfile
# fallback path (section A's fallback control) and never asks systemctl at all here.

BD_CALL_LOG="$T/bd-calls.log"
# REAL_BD is resolved to the ACTUAL binary testdb_up exported, captured now — before
# run_call_only overrides SPIRA_BD to this wrapper for the subprocess. Embedding the
# resolved value (not another read of $SPIRA_BD) is what keeps this a counting passthrough
# to the real dependency rather than a self-recursive stub.
REAL_BD="${SPIRA_BD:-bd}"
cat > "$DSTUBS/counting-bd" <<EOF
#!/usr/bin/env bash
echo "\$*" >> "$BD_CALL_LOG"
exec "$REAL_BD" "\$@"
EOF
chmod +x "$DSTUBS/counting-bd"

# THE READY SET IS THE MACHINE'S. spira-claim (bulk-ready-by-rune, inside the call pass)
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

run_call_only() {   # run_call_only <run-dir> [KEY=VAL ...]
    local run="$1"; shift
    mkdir -p "$run"
    tl_config SPIRA_RUN="$run" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$DSTUBS/counting-bd" \
        SPIRA_RUNES=builder SPIRA_SCOPE_LABEL="" SPIRA_MAX_DWARVES=2 \
        SPIRA_RUNES_DIR="$DSTUBS/runes"
    # SPIRA_DB/SPIRA_BD also passed literally: the spira-lc stub chain (lc_mirror_bd,
    # wrapped by LCBIN's counting shim) is plain bash reading them straight from its own
    # environment, never through spira-config (sfail round 3, pattern 8).
    env -i \
        PATH="$LCBIN:$DSTUBS:$PATH" HOME="$HOME" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$DSTUBS" \
        SPIRA_CALL="$DSTUBS/mock-call" \
        SPIRA_DB="$SPIRA_DB" SPIRA_BD="$DSTUBS/counting-bd" \
        "$@" \
        sentinel --call-only 2>&1
}

# builder is elastic (RUNE_ELASTIC=1, builder.rune): its fill loop is bound by the POOL,
# not by the ready count, because the mock call below never actually claims a bead — the
# same shape test-sentinel-pass.sh's own "fill" pass exercises. Pool=2 here so the call
# count this asserts (2) is unambiguous rather than an artifact of the fixture's 2 ready beads.
#
# ONE SHARED $SPIRA_RUN FOR THE WHOLE SECTION, PRIMED FIRST. conf.sh runs its own `bd
# migrate schema` the first time any script sources it against a given $SPIRA_RUN (cached
# by a stamp file there after) — orthogonal to sentinel.sh's own bd usage, but a bd call
# all the same, and counted by the same wrapper. Priming it once, behind world.halted so
# nothing is called by the prime itself, keeps that unrelated check out of the counts
# below as far as it can; the assertions themselves count "ready" calls specifically
# (never a bare call count) so a leftover connection retry cannot flip them either.
_drun="$T/run-call-only"; mkdir -p "$_drun"
: > "$_drun/world.halted"
run_call_only "$_drun" >/dev/null 2>&1
rm -f "$_drun/world.halted"

rm -f "$CALL_LOG" "$BD_CALL_LOG" "$LC_CALL_LOG"
out_d1="$(run_call_only "$_drun")"
is "D: pool=2 -> exactly 2 calls (elastic fill, bounded by the pool)" "2" "$(grep -c . "$CALL_LOG" 2>/dev/null || echo 0)"
want "D: the ready set was read from the lifecycle machine (spira-lc list)" "list" "$(cat "$LC_CALL_LOG" 2>/dev/null)"
# POSITIVE CONTROL for the zero below: the counting wrapper does see this pass's bd reads
# (spira-claim's content read of the READY beads), so a zero is not a dead log.
want "D: positive control: the pass's bd reads reach the counting wrapper" "list" "$(cat "$BD_CALL_LOG" 2>/dev/null)"
# grep -c prints 0 AND exits 1 on no match, so no `|| echo 0` here (that printed "0\n0"); the
# positive control above already proved the log exists.
is "D: ZERO bd ready calls — the call pass's ready set is the machine's" "0" "$(grep -c ' ready ' "$BD_CALL_LOG" 2>/dev/null)"
want "D: log reports the call-only pass" "call-only pass complete" "$out_d1"
nowant "D: no full-pass state line" "state: open=" "$out_d1"
nowant "D: no full-pass CHECK7c" "CHECK7c" "$out_d1"
nowant "D: no Hel" "hel:" "$out_d1"

echo
echo "D — the machine, not bd, decides: beads bd calls open but the machine holds call nothing:"
# Same store, same pool; the machine now says both beads are WORKING. bd still calls them
# open — a caller reading bd's status would fill the pool again.
printf 'sp-b1 WORKING\nsp-b2 WORKING\n' > "$LCMIRROR/states"
rm -f "$CALL_LOG" "$BD_CALL_LOG"
out_held="$(run_call_only "$_drun")"
is "held: no call when the machine has no READY row" "0" "$(grep -c . "$CALL_LOG" 2>/dev/null || echo 0)"
want "held: says nothing is ready" "nothing ready in its partition" "$out_held"
rm -f "$LCMIRROR/states"

echo
echo "D — world.halted short-circuits before the live count or any bd call:"
rm -f "$CALL_LOG" "$BD_CALL_LOG"
: > "$_drun/world.halted"
out_halted="$(run_call_only "$_drun")"
is "halted: no call happens" "0" "$(grep -c . "$CALL_LOG" 2>/dev/null || echo 0)"
is "halted: no bd 'ready' call is made" "0" "$(grep -c ' ready ' "$BD_CALL_LOG" 2>/dev/null || echo 0)"
want "halted: says so" "halted — not calling" "$out_halted"

# ============================================================================
echo
echo "E — every dwarf carries its own fast-path refill hook (ExecStopPost):"
# ============================================================================
# A call a full pass or --call-only both start (the dwarf unit itself needs no timer to
# know it exited) — the refill it wires must reach --call-only, never the full pass, or
# a dwarf exiting is right back to waiting out the 2-minute cadence this bead exists to cut.
export SPIRA_CALL="$T/bin/mock-call-noop"   # already an absolute path; created in section A
# The refill runs the sentinel BINARY (sentinel.sh is gone), resolved on PATH (sp-gypjk):
# ExecStopPost needs an absolute path, so call_argv hands it its own resolved path.
# `call_argv` is itself a `sentinel --call-argv` shim now (wave 4.27, family G,
# sp-gzmd2) — a real execution, not a pure string computation in this shell — so it
# necessarily resolves to WHATEVER "sentinel" is really on PATH, not a stand-in; the
# assertion below resolves the same name through the same PATH rather than asserting a
# fixed fake path no real binary would ever embed as itself.
sentinel_path="$(PATH="$T/bin:$PATH" command -v sentinel)"
argv="$(PATH="$T/bin:$PATH" call_argv racer | tr '\n' ' ')"
case "$argv" in
    *"--property=ExecStopPost=$T/bin/mock-call-noop --user --collect --quiet "*"--setenv=SPIRA_TOML="*" $sentinel_path --call-only"*)
        ok "call_argv: ExecStopPost refills via --call-only, not a full pass, with SPIRA_TOML" ;;
    *) bad "call_argv: ExecStopPost refills via --call-only" "got: $argv (sentinel resolved to $sentinel_path)" ;;
esac
unset SPIRA_CALL 2>/dev/null || true

tl_summary
