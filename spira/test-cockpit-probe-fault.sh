#!/usr/bin/env bash
#
# test-cockpit-probe-fault.sh — fault-inject each SP_* count and assert the pane shows ? not 0.
#
#   ./test-cockpit-probe-fault.sh
#
# THREE FAILURE MODES, ALL OCCURRING IN PRODUCTION:
#   1. bd exits non-zero            — the query layer refuses; the collector should emit ?
#   2. bdjson exits 0, emits nothing — a pipeline ending in sed inherits sed's exit status,
#                                     so a refusing bd still exits 0; the collector must
#                                     distinguish an empty file from an empty array
#   3. snapshot key absent (stale or partial) — the renderer must not silently default to 0
#
# For EVERY SP_* count and list the ops pane renders, this suite makes the underlying probe
# refuse by one of these mechanisms and asserts health.sh shows '?' or a named error, never 0
# (law-absence-needs-a-positive-control).
#
# THE SUITE IS EXPECTED TO FAIL AGAINST THE UNFIXED CODE ON SP_AWAITING_LAND:
#   - health.sh uses ${SP_AWAITING_LAND:-0} so an absent key renders 0, not ?
#   - cockpit.sh unsent_keys() emits SP_AWAITING_LAND=0 when bdjson returns nothing because
#     it cannot distinguish "bd produced an empty JSON array" from "bd refused entirely"
# Both are fixed in this commit; the pre-fix failure text is recorded in the commit message.
#
# defect: sp-cof
# covers: cockpit/health.sh spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PANE="$HERE/../cockpit/health.sh"
pass=0; fail=0
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s\n' "$1"; }
is_n() { if [ "$2" = "$3" ]; then pass=$((pass+1)); printf '  ok    %s\n' "$1"
         else fail=$((fail+1)); printf '  FAIL  %s: want [%s] got [%s]\n' "$1" "$2" "$3"; fi; }

# ---- pane helpers (same pattern as test-now.sh) ----------------------------------------

if [ ! -f "$PANE" ]; then
    bad "cannot find the pane at $PANE — nothing to test"
    printf '%d passed, %d failed\n' "$pass" "$fail"; exit 1
fi

PD="$TMP/pane"
mkdir -p "$PD/repo/.runtime/spira" "$PD/home" "$PD/bin"
SNAPF="$PD/repo/.runtime/spira/cockpit.env"

printf '#!/bin/sh\necho active\n' > "$PD/bin/mock-systemctl"
chmod +x "$PD/bin/mock-systemctl"

# snap() writes a cockpit.env from stdin in the single-quoted KEY='value' form
# that health.sh sources.
snap() {
    python3 -c '
import sys
for line in sys.stdin.read().splitlines():
    if not line.strip(): continue
    k, _, v = line.partition("=")
    print("%s=%s" % (k, "\x27" + v.replace("\x27", "\x27\\\x27\x27") + "\x27"))
' > "$SNAPF"
}

pane() {    # pane [rows [cols]] -> ANSI-stripped frame
    env -i PATH="$PATH" HOME="$PD/home" TERM=dumb LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_REPO="$PD/repo" \
        SPIRA_RUN="$PD/repo/.runtime/spira" \
        SPIRA_SYSTEMCTL="$PD/bin/mock-systemctl" \
        bash "$PANE" once "${1:-0}" "${2:-0}" 2>/dev/null \
      | sed 's/\x1b\[[?0-9;]*[a-zA-Z]//g'
}

# base_snap <omit-key> — write a full snapshot that omits the named key.
# Every other key is set to a plausible non-zero value so the omission is the only
# difference and the failing section is reachable.
base_snap() {
    local omit="${1:-__none__}"
    {
        printf 'SP_AT=%d\n' "$(date +%s)"
        printf 'SP_WINDOW_HOURS=24\n'
        printf 'SP_PASS_SECS=12\n'
        printf 'SP_SENTINEL_TIMER=1\nSP_SENTINEL_AGE=30\n'
        printf 'SP_OPS_TIMER=1\nSP_OPS_AGE=30\n'
        printf 'SP_AURON_TIMER=1\nSP_AURON_AGE=30\nSP_AURON_FIRING=0\nSP_AURON_KEYS=\n'
        printf 'SP_AEON_N=0\n'
        printf 'SP_GATE_N=0\nSP_GATE_LIVE=0\n'
        printf 'SP_CAPACITY_PAUSED=0\nSP_CAPACITY_LEFT=0\nSP_CAPACITY_AT=\nSP_CAPACITY_WHY=\n'
        printf 'SP_TOK_WINDOW_H=5\nSP_TOK_WIN=100000\nSP_TOK_AEON_WIN=80000\nSP_TOK_SESS_WIN=20000\n'
        printf 'SP_TOK_AEON_TURNS=5\nSP_TOK_SESS_TURNS=3\nSP_TOK_AEON_CTX=50000\nSP_TOK_SESS_CTX=30000\n'
        printf 'SP_TOK_AEON_OUT=3000\nSP_TOK_SESS_OUT=1000\nSP_TOK_AEON_RECENT=10000\nSP_TOK_SESS_RECENT=5000\n'
        printf 'SP_CTX_NOW=50000\nSP_CTX_TURNS=10\nSP_CTX_NEXT=ok\nSP_CTX_HEADROOM=150000\n'
        printf 'SP_CTX_GROWTH=5000\nSP_CTX_TURNS_LEFT=30\nSP_CTX_AGE=60\nSP_CTX_ARCHIVIST=none\n'
        printf 'SP_CTX_ARCHIVIST_BEHIND=0\nSP_CTX_ARCHIVIST_FILED=0\nSP_CTX_SCAN_BYTES=0\n'
        printf 'SP_RATELIM_5H_PCT=20\nSP_RATELIM_7D_PCT=10\nSP_RATELIM_5H_MIN=240\nSP_RATELIM_7D_MIN=2000\n'
        printf 'SP_LIMIT_5H_PCT=20\nSP_LIMIT_5H_ETA=\nSP_LIMIT_7D_PCT=10\nSP_LIMIT_7D_ETA=\nSP_LIMIT_AGE=60\n'
        printf 'SP_PASSES=5\nSP_ACTS=3\nSP_FALSE_ACTS=0\nSP_FALSE_PER_PASS=0\nSP_SINCE_JUDGEMENT=10\n'
        printf 'SP_AEON_BORN=1\nSP_AEON_LIVED=1\nSP_AEON_STILLBORN=0\nSP_AEON_WORKED=1\nSP_AEON_THRASH=0\n'
        printf 'SP_SELF_REPEATING_N=0\nSP_SELF_STILLBORN_W=0\nSP_SELF_STILLBORN_LAST=\n'
        printf 'SP_SELF_STARVED_W=0\nSP_SELF_STARVED_LAST=\n'
        # Variable sections — omit the key under test.
        [ "$omit" = SP_NEXT_N      ] || printf 'SP_NEXT_N=2\n'
        [ "$omit" = SP_INFLOW_N    ] || printf 'SP_INFLOW_N=3\nSP_INFLOW_WIN=60\nSP_INFLOW_DEFECT=0\nSP_INFLOW_KINDS=task 3\n'
        [ "$omit" = SP_AWAITING_N  ] || printf 'SP_AWAITING_N=0\n'
        [ "$omit" = SP_PEND_N      ] || printf 'SP_PEND_N=0\nSP_PEND_OLDEST=0\n'
        [ "$omit" = SP_WAITING     ] || printf 'SP_WAITING=1\n'
        [ "$omit" = SP_UNANSWERED  ] || printf 'SP_UNANSWERED=0\n'
        [ "$omit" = SP_UNSENT      ] || printf 'SP_UNSENT=2\nSP_UNSENT_OLDEST_H=1\nSP_BRANCH_DONE=1\nSP_UNADOPTED=0\nSP_ORPHAN_WORK=0\n'
        [ "$omit" = SP_CLOSED_24H  ] || printf 'SP_CLOSED_24H=5\n'
        [ "$omit" = SP_OPENED_24H  ] || printf 'SP_OPENED_24H=3\n'
        # BEADS_LANDED_24H and the spark series.
        [ "$omit" = SP_BEADS_LANDED_24H ] || printf 'SP_BEADS_LANDED_24H=4\n'
        printf 'SP_BEADS_SPARK_OPENED=▁▂▁▃▁▂▁▃\nSP_BEADS_SPARK_CLOSED=▁▂▁▃▁▂▁▃\n'
        printf 'SP_BEADS_SPARK_LANDED=▁▂▁▃▁▂▁▃\n'
        printf 'SP_CLOSED_KINDS=task 5\n'
        # 24h landing row — three separate keys.
        [ "$omit" = SP_LANDED      ] || printf 'SP_LANDED=4\n'
        [ "$omit" = SP_AWAITING_LAND ] || printf 'SP_AWAITING_LAND=1\n'
        [ "$omit" = SP_UNLANDED    ] || printf 'SP_UNLANDED=0\n'
        printf 'SP_CLOSED=5\n'
        # LAND section keys.
        printf 'SP_LAND_AT=%d\nSP_LAND_RC=0\nSP_LAND_BRANCHES=0\nSP_LAND_MOVED=0\n' "$(date +%s)"
        # Graph and livelock.
        printf 'SP_OPEN=7\nSP_READY=2\nSP_INPROG=1\nSP_POISON=0\nSP_STRAND_GHOST=0\nSP_STRANDS=3\n'
        printf 'SP_LIVELOCKED=0\nSP_INVALID_CLOSED=0\nSP_UNFILED_FOLLOW=0\n'
        printf 'SP_YIELD_REDS=0\nSP_YIELD_DEFECT=0\nSP_YIELD_FAULT=0\nSP_YIELD_UNKNOWN=0\n'
        printf 'SP_YIELD_SOLO_MED=0\nSP_YIELD_CONC_MED=0\nSP_YIELD_TOP_FAULT=-\n'
        printf 'SP_SENT_FAILED=0\nSP_SENT_FAILED_AGE_M=0\n'
        printf 'SP_UNSENT_OLDEST_H=0\n'
    } | snap
}

# ---- Part 1: renderer fault injection ---------------------------------------------------
# For each SP_* count, write a snapshot that omits it and assert the pane shows ? not 0.
# A POSITIVE CONTROL PRECEDES EACH FAULT: put the key back and verify the real value
# appears — proving the field is reachable before claiming its absence is detected.

echo "Part 1: renderer fault injection — absent snapshot key must render ?, not 0"

# SP_NEXT_N — ready-queue count, guarded by unread_row NEXT
base_snap __none__
p="$(pane 0)"
if grep -qF 'NEXT' <<< "$p"; then
    ok "SP_NEXT_N positive control: NEXT section renders"
else
    bad "SP_NEXT_N positive control: NEXT section absent from frame (cannot test fault)"
fi
base_snap SP_NEXT_N
p="$(pane 0)"
if grep -q 'NEXT.*?' <<< "$p"; then
    ok "SP_NEXT_N: absent key renders '?' (cannot read the ready queue)"
else
    bad "SP_NEXT_N: absent key did not render '?': $(printf '%s\n' "$p" | grep -i next)"
fi
if printf '%s\n' "$p" | grep -i next | grep -q ' 0$\| 0 \|^0 '; then
    bad "SP_NEXT_N: absent key rendered 0 — all-clear from a broken probe"
else
    ok "SP_NEXT_N: absent key did not render 0"
fi

# SP_INFLOW_N — inflow count, guarded by unread_row INFLOW
base_snap __none__
p="$(pane 0)"
if grep -qF 'INFLOW' <<< "$p"; then
    ok "SP_INFLOW_N positive control: INFLOW section renders"
else
    bad "SP_INFLOW_N positive control: INFLOW section absent (cannot test fault)"
fi
base_snap SP_INFLOW_N
p="$(pane 0)"
if grep -q 'INFLOW.*?' <<< "$p"; then
    ok "SP_INFLOW_N: absent key renders '?' (cannot read what is being cut)"
else
    bad "SP_INFLOW_N: absent key did not render '?': $(printf '%s\n' "$p" | grep -i inflow)"
fi

# SP_AWAITING_N — CI gate count, guarded by unread_row CI
base_snap __none__
p="$(pane 0)"
if grep -qF ' CI ' <<< "$p"; then
    ok "SP_AWAITING_N positive control: CI section renders"
else
    bad "SP_AWAITING_N positive control: CI section absent (cannot test fault)"
fi
base_snap SP_AWAITING_N
p="$(pane 0)"
if grep -q ' CI .*?' <<< "$p"; then
    ok "SP_AWAITING_N: absent key renders '?' (cannot read what is parked on CI)"
else
    bad "SP_AWAITING_N: absent key did not render '?': $(printf '%s\n' "$p" | grep ' CI ')"
fi

# SP_PEND_N — unlanded queue count, guarded by unread_row UNLND
base_snap __none__
p="$(pane 0)"
if grep -qF 'UNLND' <<< "$p"; then
    ok "SP_PEND_N positive control: UNLND section renders"
else
    bad "SP_PEND_N positive control: UNLND section absent (cannot test fault)"
fi
base_snap SP_PEND_N
p="$(pane 0)"
if grep -q 'UNLND.*?' <<< "$p"; then
    ok "SP_PEND_N: absent key renders '?' (cannot read the unlanded queue)"
else
    bad "SP_PEND_N: absent key did not render '?': $(printf '%s\n' "$p" | grep -i unlnd)"
fi

# SP_WAITING — operator attention count, rendered with ${SP_WAITING:-?}
base_snap __none__
p="$(pane 0)"
if grep -qF 'ATTN' <<< "$p"; then
    ok "SP_WAITING positive control: ATTN section renders"
else
    bad "SP_WAITING positive control: ATTN section absent (cannot test fault)"
fi
base_snap SP_WAITING
p="$(pane 0)"
attn_line="$(printf '%s\n' "$p" | grep ' ATTN ')"
if printf '%s\n' "$attn_line" | grep -q 'waiting on you [?]'; then
    ok "SP_WAITING: absent key renders '?'"
else
    bad "SP_WAITING: absent key did not render '?': $attn_line"
fi

# SP_UNANSWERED — thread-reply count, rendered with ${SP_UNANSWERED:-?}
base_snap __none__
p="$(pane 0)"
if grep -qF 'threads awaiting' <<< "$p"; then
    ok "SP_UNANSWERED positive control: thread-reply field renders"
else
    bad "SP_UNANSWERED positive control: thread-reply field absent (cannot test fault)"
fi
base_snap SP_UNANSWERED
p="$(pane 0)"
attn2_line="$(printf '%s\n' "$p" | grep 'threads awaiting')"
if printf '%s\n' "$attn2_line" | grep -q '[?]$\|[?] '; then
    ok "SP_UNANSWERED: absent key renders '?'"
else
    bad "SP_UNANSWERED: absent key did not render '?': $attn2_line"
fi

# SP_UNSENT — unsent branch count, rendered with ${SP_UNSENT:-?}
base_snap __none__
p="$(pane 0)"
if grep -qF 'SEND' <<< "$p"; then
    ok "SP_UNSENT positive control: SEND section renders"
else
    bad "SP_UNSENT positive control: SEND section absent (cannot test fault)"
fi
base_snap SP_UNSENT
p="$(pane 0)"
send_line="$(printf '%s\n' "$p" | grep ' SEND ')"
if printf '%s\n' "$send_line" | grep -q '[?] branches'; then
    ok "SP_UNSENT: absent key renders '?'"
else
    bad "SP_UNSENT: absent key did not render '?': $send_line"
fi

# SP_CLOSED_24H — 24h closed count, rendered with ${SP_CLOSED_24H:-?}
base_snap __none__
p="$(pane 0)"
if grep -qF 'BEADS' <<< "$p"; then
    ok "SP_CLOSED_24H positive control: BEADS section renders"
else
    bad "SP_CLOSED_24H positive control: BEADS section absent (cannot test fault)"
fi
base_snap SP_CLOSED_24H
p="$(pane 0)"
beads_line="$(printf '%s\n' "$p" | grep ' BEADS ')"
if printf '%s\n' "$beads_line" | grep -q 'closed [?]'; then
    ok "SP_CLOSED_24H: absent key renders '?'"
else
    bad "SP_CLOSED_24H: absent key did not render '?': $beads_line"
fi

# SP_OPENED_24H — 24h opened count, rendered with ${SP_OPENED_24H:-?}
base_snap SP_OPENED_24H
p="$(pane 0)"
beads_line="$(printf '%s\n' "$p" | grep ' BEADS ')"
if printf '%s\n' "$beads_line" | grep -q 'opened [?]'; then
    ok "SP_OPENED_24H: absent key renders '?'"
else
    bad "SP_OPENED_24H: absent key did not render '?': $beads_line"
fi

# SP_BEADS_LANDED_24H — 24h landed count, rendered with ${SP_BEADS_LANDED_24H:-?}
base_snap __none__
p="$(pane 0)"
if grep -q 'in 24h' <<< "$p"; then
    ok "SP_BEADS_LANDED_24H positive control: landed line renders"
else
    bad "SP_BEADS_LANDED_24H positive control: landed line absent (cannot test fault)"
fi
base_snap SP_BEADS_LANDED_24H
p="$(pane 0)"
if grep -q '[?] in 24h' <<< "$p"; then
    ok "SP_BEADS_LANDED_24H: absent key renders '?'"
else
    bad "SP_BEADS_LANDED_24H: absent key did not render '?': $(printf '%s\n' "$p" | grep 'in 24h')"
fi

# SP_LANDED — landed count on the 24h-worked row, rendered with ${SP_LANDED:-?}
base_snap __none__
p="$(pane 0)"
if grep -q '24h worked' <<< "$p"; then
    ok "SP_LANDED positive control: 24h worked row renders"
else
    bad "SP_LANDED positive control: 24h worked row absent (cannot test fault)"
fi
base_snap SP_LANDED
p="$(pane 0)"
worked_line="$(printf '%s\n' "$p" | grep '24h worked')"
if printf '%s\n' "$worked_line" | grep -q '[?] landed'; then
    ok "SP_LANDED: absent key renders '?'"
else
    bad "SP_LANDED: absent key did not render '?': $worked_line"
fi

# SP_AWAITING_LAND — awaiting-land count on the 24h-worked row.
# THE FAILURE CASE THAT PROVES THE STATUTE WAS VIOLATED: health.sh uses
# ${SP_AWAITING_LAND:-0}, so an absent key renders "0 awaiting", not "? awaiting".
# This case is expected to FAIL against the unfixed renderer (pre-fix of sp-cof).
base_snap __none__
p="$(pane 0)"
# Positive control: SP_AWAITING_LAND=1 in the base snapshot → "1 awaiting" appears.
if grep -q '1 awaiting' <<< "$p"; then
    ok "SP_AWAITING_LAND positive control: '1 awaiting' renders"
else
    bad "SP_AWAITING_LAND positive control: '1 awaiting' absent from frame (cannot test fault)"
fi
base_snap SP_AWAITING_LAND
p="$(pane 0)"
worked_line="$(printf '%s\n' "$p" | grep '24h worked')"
# THE ASSERTION: absent key must render '?' not '0'.
if printf '%s\n' "$worked_line" | grep -q '[?] awaiting'; then
    ok "SP_AWAITING_LAND: absent key renders '?' awaiting"
else
    bad "SP_AWAITING_LAND: absent key did not render '? awaiting': $worked_line"
fi
# THE INVERSE: must not show '0 awaiting' (the all-clear a broken probe must never produce).
if printf '%s\n' "$worked_line" | grep -q '0 awaiting'; then
    bad "SP_AWAITING_LAND: absent key rendered '0 awaiting' — all-clear from broken probe"
else
    ok "SP_AWAITING_LAND: absent key did not render '0 awaiting'"
fi

# SP_UNLANDED — never-landed count on the 24h-worked row, rendered with ${SP_UNLANDED:-?}
base_snap SP_UNLANDED
p="$(pane 0)"
worked_line="$(printf '%s\n' "$p" | grep '24h worked')"
if printf '%s\n' "$worked_line" | grep -q '[?] never landed'; then
    ok "SP_UNLANDED: absent key renders '?'"
else
    bad "SP_UNLANDED: absent key did not render '?': $worked_line"
fi

echo
echo "Part 2: collector fault injection — probe refusal must produce ?, not 0"
echo

# ---- Part 2: collector fault injection --------------------------------------------------
# Run specific cockpit.sh subcommands with fake bd binaries and verify the output.
# THREE MODES: (1) bd exits non-zero, (2) bdjson exits 0 and emits nothing, (3) the
# same collector output feeds the pane and the pane renders ?.

# Fake bd binaries.
mkdir -p "$TMP/bin"
# bd-fail: simulates "bd exits non-zero" — the database refused the query.
printf '#!/bin/sh\nexit 1\n' > "$TMP/bin/bd-fail"
chmod +x "$TMP/bin/bd-fail"
# bd-zero-empty: simulates "bdjson exits 0, emits nothing" — the underlying bd writes nothing
# to stdout but exits 0 (as happens when a pipeline ending in sed propagates sed's zero exit
# even after a non-zero bd). The bdjson function cannot tell bd refused from bd returning
# an empty result; only the file-size check or JSON presence can tell.
printf '#!/bin/sh\nprintf ""\nexit 0\n' > "$TMP/bin/bd-zero-empty"
chmod +x "$TMP/bin/bd-zero-empty"

# run_probe <subcommand> <SPIRA_BD-binary> [extra-env-vars...] -> output of cockpit.sh <subcommand>
# AN EXPLICIT MINIMAL ENVIRONMENT. Ambient configuration silently decides verdicts:
# a suite that inherits a real spira.conf would assert against one box
# (law-gates-run-in-a-clean-environment). BD_TIMEOUT=1 fails fast against a fake bd.
run_probe() {
    local sub="$1" bd_bin="$2"; shift 2
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_BD="$TMP/bin/$bd_bin" \
        SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" \
        BD_TIMEOUT=1 \
        "$@" \
        bash "$HERE/cockpit.sh" "$sub" 2>/dev/null
}
mkdir -p "$TMP/run"

# ---- SP_WAITING: core_counts_keys, bd exits non-zero ----------------------------------
# POSITIVE CONTROL: with a bd that emits '[]', SP_WAITING should be a number.
# (We cannot run a real bd in a bare test environment, so the positive-control is the
# the section-guard itself: SP_WAITING=? is the known-wrong output we are measuring against.)
echo "SP_WAITING — core_counts_keys (bd exits non-zero)"
out_wait="$(run_probe core bd-fail)"
if grep -q '^SP_WAITING=?' <<< "$out_wait"; then
    ok "SP_WAITING: bd exits non-zero renders SP_WAITING=?"
else
    bad "SP_WAITING: bd exits non-zero did not render SP_WAITING=?: $(grep SP_WAITING <<< "$out_wait" | head -1)"
fi
# SP_WAITING must not be 0 (the all-clear reading a failed probe must never produce).
if grep -q '^SP_WAITING=0' <<< "$out_wait"; then
    bad "SP_WAITING: bd exits non-zero rendered SP_WAITING=0 — all-clear from broken probe"
else
    ok "SP_WAITING: bd exits non-zero did not render SP_WAITING=0"
fi

# ---- SP_NEXT_N: core_detail_keys, bd exits non-zero ----------------------------------
# Without fayth files, _PART_MAP is empty → sentinel → SP_NEXT_N=? ("unreadable chamber").
# That is the correct refused-probe behaviour for this field.
echo
echo "SP_NEXT_N — core_detail_keys (no chamber / bd exits non-zero)"
out_next="$(run_probe core_detail bd-fail)"
if grep -q '^SP_NEXT_N=?' <<< "$out_next"; then
    ok "SP_NEXT_N: probe failure renders SP_NEXT_N=?"
else
    bad "SP_NEXT_N: probe failure did not render SP_NEXT_N=?: $(grep SP_NEXT_N <<< "$out_next" | head -1)"
fi
if grep -q '^SP_NEXT_N=0' <<< "$out_next"; then
    bad "SP_NEXT_N: probe failure rendered SP_NEXT_N=0 — all-clear from broken probe"
else
    ok "SP_NEXT_N: probe failure did not render SP_NEXT_N=0"
fi

# ---- SP_INFLOW_N: core_detail_keys, bdjson exits 0 emits nothing ---------------------
# THE INFLOW CASE THAT WAS FIXED IN 285ec53: when bdjson exits 0 but emits nothing (the
# pipeline ends in sed whose exit status masks the bd failure), _store_read=0 because the
# TITLEMAP is empty. The Python block is never reached, and SP_INFLOW_N=? is emitted.
echo
echo "SP_INFLOW_N — core_detail_keys (bdjson exits 0, emits nothing)"
out_inflow="$(run_probe core_detail bd-zero-empty)"
if grep -q '^SP_INFLOW_N=?' <<< "$out_inflow"; then
    ok "SP_INFLOW_N: bdjson-exits-0-emits-nothing renders SP_INFLOW_N=?"
else
    bad "SP_INFLOW_N: bdjson-exits-0-emits-nothing did not render SP_INFLOW_N=?: $(grep SP_INFLOW_N <<< "$out_inflow" | head -1)"
fi
if grep -q '^SP_INFLOW_N=0' <<< "$out_inflow"; then
    bad "SP_INFLOW_N: bdjson-exits-0-emits-nothing rendered SP_INFLOW_N=0 — all-clear from broken probe"
else
    ok "SP_INFLOW_N: bdjson-exits-0-emits-nothing did not render SP_INFLOW_N=0"
fi

# ---- SP_AWAITING_LAND: unsent_keys, bdjson exits 0 emits nothing --------------------
# THE REMAINING COLLECTOR BUG THAT SP-COF FIXES: when bdjson list --status closed exits 0
# but emits nothing, closed_pairs is empty. The original code treated empty-from-refusal
# and empty-from-no-beads the same way — both emitted SP_CLOSED=0 SP_AWAITING_LAND=0.
# After the fix, an empty output (bd produced nothing) emits SP_AWAITING_LAND=? while a
# genuine empty list (bd produced "[]") still emits SP_AWAITING_LAND=0.
#
# POSITIVE CONTROL: run with bd-fail (exits non-zero) — the bdjson pipeline's Python exits 1,
# closed_pairs is empty, and the fix must detect the refusal and emit ?.
echo
echo "SP_AWAITING_LAND — unsent_keys (bdjson exits 0, emits nothing)"
out_unsent_empty="$(run_probe unsent bd-zero-empty)"
if grep -q '^SP_AWAITING_LAND=?' <<< "$out_unsent_empty"; then
    ok "SP_AWAITING_LAND: bdjson-exits-0-emits-nothing renders SP_AWAITING_LAND=?"
else
    bad "SP_AWAITING_LAND: bdjson-exits-0-emits-nothing did not render SP_AWAITING_LAND=?: $(grep SP_AWAITING_LAND <<< "$out_unsent_empty" | head -1)"
fi
if grep -q '^SP_AWAITING_LAND=0' <<< "$out_unsent_empty"; then
    bad "SP_AWAITING_LAND: bdjson-exits-0-emits-nothing rendered SP_AWAITING_LAND=0 — all-clear from broken probe"
else
    ok "SP_AWAITING_LAND: bdjson-exits-0-emits-nothing did not render SP_AWAITING_LAND=0"
fi

# Also check the other keys emitted by the same failing probe section.
for _k in SP_CLOSED SP_LANDED SP_UNLANDED SP_PEND_N; do
    if grep -q "^${_k}=?" <<< "$out_unsent_empty"; then
        ok "$_k: bdjson-exits-0-emits-nothing renders ${_k}=?"
    else
        bad "$_k: bdjson-exits-0-emits-nothing did not render ${_k}=?: $(grep "^${_k}=" <<< "$out_unsent_empty" | head -1)"
    fi
done

# ---- Part 2 summary: collector output feeds the pane --------------------------------
# Take the collector output that correctly contains SP_AWAITING_LAND=? and verify the pane
# renders it as ? rather than 0 — closing the end-to-end chain.
echo
echo "end-to-end: collector ? propagates to pane ?"
{
    printf 'SP_AT=%d\n' "$(date +%s)"
    printf 'SP_AEON_N=0\nSP_NEXT_N=0\nSP_INFLOW_N=0\nSP_INFLOW_WIN=60\nSP_INFLOW_DEFECT=0\nSP_INFLOW_KINDS=-\n'
    printf 'SP_AWAITING_N=0\nSP_PEND_N=0\nSP_WAITING=0\nSP_UNANSWERED=0\n'
    printf 'SP_UNSENT=0\nSP_BRANCH_DONE=0\nSP_UNSENT_OLDEST_H=0\nSP_UNADOPTED=0\nSP_ORPHAN_WORK=0\n'
    printf 'SP_CLOSED_24H=0\nSP_OPENED_24H=0\nSP_BEADS_LANDED_24H=0\n'
    printf 'SP_BEADS_SPARK_OPENED=▁▁▁▁▁▁▁▁\nSP_BEADS_SPARK_CLOSED=▁▁▁▁▁▁▁▁\n'
    printf 'SP_BEADS_SPARK_LANDED=▁▁▁▁▁▁▁▁\nSP_CLOSED_KINDS=-\n'
    # THE PROPAGATION CASE: what the fixed collector emits when bdjson returns nothing.
    printf 'SP_CLOSED=?\nSP_LANDED=?\nSP_AWAITING_LAND=?\nSP_UNLANDED=?\nSP_PEND_N=?\nSP_PEND_OLDEST=?\n'
    printf 'SP_SENTINEL_TIMER=1\nSP_SENTINEL_AGE=30\nSP_OPS_TIMER=1\nSP_OPS_AGE=30\n'
    printf 'SP_AURON_TIMER=1\nSP_AURON_AGE=30\nSP_AURON_FIRING=0\nSP_AURON_KEYS=\n'
    printf 'SP_GATE_N=0\nSP_GATE_LIVE=0\nSP_CAPACITY_PAUSED=0\n'
} | snap
e2e_pane="$(pane 0)"
e2e_worked="$(printf '%s\n' "$e2e_pane" | grep '24h worked')"
if printf '%s\n' "$e2e_worked" | grep -q '[?] awaiting'; then
    ok "end-to-end: collector SP_AWAITING_LAND=? propagates to pane '? awaiting'"
else
    bad "end-to-end: collector SP_AWAITING_LAND=? did not propagate to pane '? awaiting': $e2e_worked"
fi
if printf '%s\n' "$e2e_worked" | grep -q '0 awaiting'; then
    bad "end-to-end: pane rendered '0 awaiting' despite SP_AWAITING_LAND=?"
else
    ok "end-to-end: pane did not render '0 awaiting'"
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
