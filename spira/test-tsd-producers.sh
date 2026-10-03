#!/usr/bin/env bash
#
# test-tsd-producers.sh — the run/tsd/ producers sp-69m85 adds: aeon-session (aeon.sh's
# ledger_done), slots (the cockpit collector) and sentinel-phase (sentinel.sh's own CHECK
# timings).
#
# Case 3, round (_tsd_round_phase and its build/corpus/attribute/rerun/land/publish
# whitelist), was retired at sp-27hsi: re-grepped dead across the whole tree (no bash or
# Rust caller left at all, not even the "whitelist itself is what remains to prove" shipped
# call site this suite used to stand in for), so the whole case is deleted rather than kept
# as a pure-function check with nothing behind it. _tsd_escape, the tsd-producers family's
# other whitelist-guarded function, stays live (escape-classify.sh calls it) and is
# untouched — this suite never covered it anyway.
#
# WHAT THIS SUITE CHECKS.
#   1. aeon-session: ledger_done (aeon.sh) appends a row carrying bead/fayth/rc/status and
#      the same wall_s/api_s/turns/cost_usd session_result_fields already computed for its
#      own ledger line — best-effort when tsd-write fails.
#   2. slots: _tsd_slots_sample (lib.sh) turns a cockpit-collect fragment into one row, an
#      absent key renders "?" rather than a silent 0; slots_keys' (cockpit-collect) fleet
#      ceiling comes from config alone, never a database call.
#   4. sentinel-phase: a real sentinel.sh pass (Dolt fixture) writes one row per CHECK, and
#      their sum is within 5% of the pass's own measured wall time (the design's acceptance
#      criterion, verbatim). The sentinel binary writes this family itself now (pass.rs,
#      direct tsd-write calls) — _tsd_sentinel_phase (lib.sh) has no caller left either and
#      is retired alongside _tsd_round_phase, but nothing here called it directly to begin
#      with, so this case needed no change.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): every refusal is paired with the
# accepting case, and every empty/absent case is paired with a real write.
#
# tier: T3
# covers: spira/lib.sh aeon/src/* sentinel/src/* cockpit-collect/src/*
#         testenv/src/* reconciler-engine/src/io.rs
#         reconciler/src/main.rs reconciler-flow/src/main.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

printf 'test-tsd-producers.sh\n'

T="$(mktemp -d)"; trap 'testdb_drop 2>/dev/null; rm -rf "$T"' EXIT INT TERM

# ── tsd-write (law-absence-needs-a-positive-control: no binary, no suite) ─────────────────
# tsd-write is the tree's own build, on the suite's PATH (sp-gypjk) — never built here.
command -v tsd-write >/dev/null 2>&1 || bail "tsd-write is not on PATH"
TSD_BIN=tsd-write

jpy() {  # jpy <file> <python-expr-on-"rows"> — rows is a list of parsed JSON lines
    python3 -c '
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
print(eval(sys.argv[2]))
' "$1" "$2"
}

# ============================================================================================
printf '\n%s\n' "1. aeon-session: RETIRED with aeon.sh"
# ledger_done was extracted from aeon.sh's source; aeon.sh is gone (the Rust cutover) and the
# aeon binary's ledger and tsd rows are covered by `cargo test -p aeon`.

# ============================================================================================
printf '\n%s\n' "2. slots: _tsd_slots_sample (lib.sh) and slots_keys' config-only ceiling"
# ============================================================================================
RUN3="$T/run3"; mkdir -p "$RUN3"
(
    export SPIRA_RUN="$RUN3"
    set -uo pipefail
    . "$HERE/lib.sh"
    FRAG="$T/slots.env"
    printf '_PROBE_AT=123\n_PROBE_STATUS=ok\n_PROBE_KILLED=0\nSP_SLOTS_LIVE=3\nSP_SLOTS_CEILING=8\nSP_SLOTS_LANES_LIVE=1\nSP_SLOTS_READY=12\nSP_SLOTS_CAPACITY_PAUSED=0\n' > "$FRAG"
    _tsd_slots_sample "$FRAG"
)
FAM3="$RUN3/tsd/slots.jsonl"
[ -f "$FAM3" ] && ok "slots row appended" || bad "MUST-FAIL CHECK: no slots row (old collect.sh behaviour)"
if [ -f "$FAM3" ]; then
    is "slots: live"            "3"  "$(jpy "$FAM3" 'rows[0]["live"]')"
    is "slots: ceiling"         "8"  "$(jpy "$FAM3" 'rows[0]["ceiling"]')"
    is "slots: lanes_live"      "1"  "$(jpy "$FAM3" 'rows[0]["lanes_live"]')"
    is "slots: ready"           "12" "$(jpy "$FAM3" 'rows[0]["ready"]')"
    is "slots: capacity_paused" "0"  "$(jpy "$FAM3" 'rows[0]["capacity_paused"]')"
fi

# Positive control: a fragment from before every key existed (a probe that has never
# reached "ok") renders the missing ones "?", never a silent 0 that reads as an empty fleet.
RUN3B="$T/run3b"; mkdir -p "$RUN3B"
(
    export SPIRA_RUN="$RUN3B"
    . "$HERE/lib.sh"
    FRAG="$T/slots-empty.env"
    printf '_PROBE_AT=0\n_PROBE_STATUS=never\n_PROBE_KILLED=0\n' > "$FRAG"
    _tsd_slots_sample "$FRAG"
)
is "slots: an absent key renders '?', not 0" "?" "$(jpy "$RUN3B/tsd/slots.jsonl" 'rows[0]["live"]')"

# slots_keys' fleet ceiling is config-only: SPIRA_MAX_LIVE_AEONS wins outright over pool +
# lane caps, and without it the sum is used — neither figure ever comes from a bd query.
#
# sp-kt4l3: slots_keys is cockpit-collect's own Rust `slots_keys()` now, not a bash function
# to extract and source — this drives the real compiled probe (`cockpit-collect probe
# slots`), which is a stronger check than the extraction ever was (no risk of the extracted
# text drifting from what actually runs).
#
# EMPTY_HOME carries a symlinked lib.sh (cockpit-collect's `io::lib_call` bridge sources
# "$SPIRA_HOME/lib.sh" exactly as the bash did) but no chamber/, so fayth_names finds
# nothing and SPIRA_FAYTHS="" resolves to zero fayths rather than the real chamber's.
EMPTY_HOME="$T/empty-home"; mkdir -p "$EMPTY_HOME"
ln -sf "$HERE/lib.sh" "$EMPTY_HOME/lib.sh"
ceiling_of() {
    (
        export SPIRA_RUN="$T/no-such-run" SPIRA_HOME="$EMPTY_HOME" SPIRA_FAYTHS="" SPIRA_SUMMON=test-stub
        export SPIRA_MAX_AEONS="$1" SPIRA_MAX_LIVE_AEONS="${2:-}"
        cockpit-collect probe slots 2>/dev/null | awk -F= '/^SP_SLOTS_CEILING=/{print $2}'
    )
}
is "slots_keys: ceiling = pool + lane caps (0 here) when SPIRA_MAX_LIVE_AEONS is unset" \
   "5" "$(ceiling_of 5 "")"
is "slots_keys: SPIRA_MAX_LIVE_AEONS overrides the pool+lanes sum" \
   "2" "$(ceiling_of 5 2)"

# ============================================================================================
printf '\n%s\n' "4. sentinel-phase: a real pass's summed CHECK rows are within 5% of its wall time"
# ============================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-tsd-producers
testdb_up tsd_producers || { echo "test-tsd-producers: could not build fixture"; exit 1; }
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-epic2","title":"epic","status":"open","issue_type":"epic","labels":["plan"]}
{"id":"sp-c1","title":"bead 1","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-c2","title":"bead 2","status":"open","issue_type":"task","labels":["plan"]}
JSONL

SP_STUBS="$T/sp-stubs"
mkdir -p "$SP_STUBS"
for _s in pilgrimage.sh reflect.sh; do
    printf '#!/bin/sh\n' > "$SP_STUBS/$_s"; chmod +x "$SP_STUBS/$_s"
done
# strand ALSO backs lib.sh's aeon_alive/aeon_count/aeons_live_total/aeons_live_lanes shims
# now (wave 4.23, sp-0ffox) — a bare no-op here would make `$(aeon_count ...)` inside
# lib.sh's pass-start summary print nothing, not "0", and blow up the arithmetic that adds
# it. A plain no-op is still correct for strand's OWN verbs (report/check/throttle-state),
# which is all this suite needs from it.
cat > "$SP_STUBS/strand" <<'STRANDSTUB'
#!/bin/sh
case "$1" in
    aeon-alive) exit 1 ;;
    aeon-count|aeons-live-total|aeons-live-lanes) printf '0' ;;
esac
STRANDSTUB
chmod +x "$SP_STUBS/strand"
# THE SENTINEL IS A BINARY (sentinel.sh is gone): it sources lib.sh from SPIRA_HOME.
for _s in lib.sh conf.sh suite-covers.sh conf.d; do ln -s "$HERE/$_s" "$SP_STUBS/$_s"; done
printf '#!/bin/sh\necho inactive\n' > "$SP_STUBS/mock-systemctl"; chmod +x "$SP_STUBS/mock-systemctl"
printf '#!/bin/sh\nexit 0\n'        > "$SP_STUBS/mock-launch";    chmod +x "$SP_STUBS/mock-launch"
printf '#!/bin/sh\nexit 0\n'        > "$SP_STUBS/mock-notify";    chmod +x "$SP_STUBS/mock-notify"
printf '#!/bin/sh\necho summoned >> "%s/summon.log"\n' "$T" > "$SP_STUBS/mock-summon"; chmod +x "$SP_STUBS/mock-summon"
printf '#!/bin/sh\n' > "$SP_STUBS/sending"; chmod +x "$SP_STUBS/sending"
touch "$SP_STUBS/repo-map"
ln -s "$HERE/chamber" "$SP_STUBS/chamber"

SP_RUN="$T/sp-run"; mkdir -p "$SP_RUN"
_pass_t0=$(date +%s)
out_pass="$(env -i \
    PATH="$SP_STUBS:$PATH" HOME="$HOME" \
    SPIRA_HOME="$SP_STUBS" \
    SPIRA_RUN="$SP_RUN" \
    SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="$SPIRA_BD" \
    SPIRA_PATH="$SPIRA_PATH" \
    SPIRA_SKIP_RECLAIM=1 \
    SPIRA_LAND_STALE=999999 \
    SPIRA_SYSTEMCTL="$SP_STUBS/mock-systemctl" \
    SPIRA_LAUNCH="$SP_STUBS/mock-launch" \
    SPIRA_SUMMON="$SP_STUBS/mock-summon" \
    SPIRA_NOTIFY="$SP_STUBS/mock-notify" \
    SPIRA_FAYTHS=builder SPIRA_SCOPE_LABEL= SPIRA_MAX_AEONS=2 \
    sentinel 2>&1)"
rc=$?
_pass_wall=$(( $(date +%s) - _pass_t0 ))
is   "sentinel pass exits 0"                       "0"            "$rc"
want "sentinel pass runs to completion (SPIRA_SKIP_RECLAIM forces open=0)" \
     "pass complete — " "$out_pass"

FAM_SP="$SP_RUN/tsd/sentinel-phase.jsonl"
[ -f "$FAM_SP" ] && ok "sentinel-phase rows appended" \
                  || bad "MUST-FAIL CHECK: no sentinel-phase rows (old sentinel.sh behaviour)"
if [ -f "$FAM_SP" ]; then
    _n_rows="$(jpy "$FAM_SP" 'len(rows)')"
    [ "${_n_rows:-0}" -gt 5 ] && ok "more than one CHECK's row was written ($_n_rows rows)" \
                                || bad "too few sentinel-phase rows" "$_n_rows"
    is "sentinel-phase: every row names the same pass" \
       "1" "$(jpy "$FAM_SP" 'len(set(r["pass"] for r in rows))')"
    _sum="$(jpy "$FAM_SP" 'sum(r["secs"] for r in rows)')"
    # +/-2s absolute floor: EPOCHSECONDS is integer-second precision, and both this pass's
    # own bash startup (before the phase clock starts) and this test's outer `date +%s`
    # calls (before and after the pass) add slop that 5% alone would not cover on a fixture
    # this small.
    _tol=$(( _pass_wall * 5 / 100 )); [ "$_tol" -lt 2 ] && _tol=2
    _diff=$(( _sum - _pass_wall )); [ "$_diff" -lt 0 ] && _diff=$(( 0 - _diff ))
    if [ "$_diff" -le "$_tol" ]; then
        ok "summed CHECK phases (${_sum}s) are within ${_tol}s of the pass's wall time (${_pass_wall}s)"
    else
        bad "summed CHECK phases vs wall time" "sum=${_sum}s wall=${_pass_wall}s tol=${_tol}s"
    fi
fi

tl_summary
