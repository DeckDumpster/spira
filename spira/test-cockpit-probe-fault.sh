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
# PART 1 AND PART 3 are ONE health.sh PROCESS. Every fixture snapshot for the renderer
# side is written to its own file first, and `health.sh render-many <dir>` (docs/test-
# plan/cockpit-observability.md, section 5 — cluster 8, UC-21) renders all of them in a
# single process instead of one `health.sh once` per case (1,844 lines + conf.sh, each).
#
# defect: sp-cof
# covers: cockpit/health.sh spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
PANE="$HERE/../cockpit/health.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---- pane helpers (same pattern as test-now.sh) ----------------------------------------

if [ ! -f "$PANE" ]; then
    bail "cannot find the pane at $PANE — nothing to test"
fi

PD="$TMP/pane"
mkdir -p "$PD/repo/.runtime/spira" "$PD/home" "$PD/bin"
FIXDIR="$TMP/fixtures"; mkdir -p "$FIXDIR"

printf '#!/bin/sh\necho active\n' > "$PD/bin/mock-systemctl"
chmod +x "$PD/bin/mock-systemctl"

# snap_to <name> — write a fixture cockpit.env from stdin, named <name>.env in FIXDIR, in the
# single-quoted KEY='value' form that health.sh sources.
snap_to() {
    local name="$1"
    python3 -c '
import sys
for line in sys.stdin.read().splitlines():
    if not line.strip(): continue
    k, _, v = line.partition("=")
    print("%s=%s" % (k, "\x27" + v.replace("\x27", "\x27\\\x27\x27") + "\x27"))
' > "$FIXDIR/$name.env"
}

# base_snap_to <name> <omit-key> — write a full snapshot named <name>.env that omits the named
# key. Every other key is set to a plausible non-zero value so the omission is the only
# difference and the failing section is reachable.
base_snap_to() {
    local name="$1" omit="${2:-__none__}"
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
        # QUEUE section guard: batch=0, next=0 so omitting SP_QUEUE_DEPTH triggers unread_row QUEUE.
        [ "$omit" = SP_QUEUE_DEPTH ] || printf 'SP_QUEUE_DEPTH=5\n'
        printf 'SP_QUEUE_EJECTED=0\nSP_QUEUE_RED=0\n'
        printf 'SP_QUEUE_BATCH_PR=0\nSP_QUEUE_BATCH_AGE=0\nSP_QUEUE_BATCH_N=0\n'
        printf 'SP_QUEUE_NEXT_N=0\nSP_QUEUE_NEXT_MAX=8\nSP_QUEUE_QUARANTINE_N=0\n'
        [ "$omit" = SP_WAITING     ] || printf 'SP_WAITING=1\n'
        [ "$omit" = SP_UNANSWERED  ] || printf 'SP_UNANSWERED=0\n'
        [ "$omit" = SP_MAIL_UNREAD ] || printf 'SP_MAIL_UNREAD=2\nSP_MAIL_N=0\nSP_MAIL_OLDEST_AGE=-\n'
        [ "$omit" = SP_UNSENT      ] || printf 'SP_UNSENT=2\nSP_UNSENT_OLDEST_H=1\nSP_BRANCH_DONE=1\nSP_UNADOPTED=0\nSP_ORPHAN_WORK=0\n'
        [ "$omit" = SP_CLOSED_24H  ] || printf 'SP_CLOSED_24H=5\n'
        [ "$omit" = SP_OPENED_24H  ] || printf 'SP_OPENED_24H=3\n'
        # BEADS_LANDED_24H and the spark series.
        [ "$omit" = SP_BEADS_LANDED_24H ] || printf 'SP_BEADS_LANDED_24H=4\n'
        printf 'SP_BEADS_SPARK_OPENED=▁▂▁▃▁▂▁▃\nSP_BEADS_SPARK_CLOSED=▁▂▁▃▁▂▁▃\n'
        printf 'SP_BEADS_SPARK_LANDED=▁▂▁▃▁▂▁▃\n'
        printf 'SP_CLOSED_KINDS=task 5\n'
        # 24h worked row — three separate keys: SP_LANDED, SP_QUEUE_DEPTH (above), SP_UNLANDED_N.
        [ "$omit" = SP_LANDED      ] || printf 'SP_LANDED=4\n'
        [ "$omit" = SP_UNLANDED_N  ] || printf 'SP_UNLANDED_N=1\n'
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
    } | snap_to "$name"
}

# ---- Part 1 fixtures: one file per case, all rendered in one process below --------------

base_snap_to full __none__
base_snap_to omit-SP_NEXT_N          SP_NEXT_N
base_snap_to omit-SP_INFLOW_N        SP_INFLOW_N
base_snap_to omit-SP_AWAITING_N      SP_AWAITING_N
base_snap_to omit-SP_QUEUE_DEPTH     SP_QUEUE_DEPTH
base_snap_to omit-SP_WAITING         SP_WAITING
base_snap_to omit-SP_UNANSWERED      SP_UNANSWERED
base_snap_to omit-SP_MAIL_UNREAD     SP_MAIL_UNREAD
base_snap_to omit-SP_UNSENT          SP_UNSENT
base_snap_to omit-SP_CLOSED_24H      SP_CLOSED_24H
base_snap_to omit-SP_OPENED_24H      SP_OPENED_24H
base_snap_to omit-SP_BEADS_LANDED_24H SP_BEADS_LANDED_24H
base_snap_to omit-SP_LANDED          SP_LANDED
base_snap_to omit-SP_UNLANDED_N      SP_UNLANDED_N

# ---- Part 3 fixtures: collector skew — SP_COLLECTOR_REV renders 'coll <rev>', not ? -----

base_snap_to arc-positive __none__
printf "SP_TOK_ARC_WIN='10000'\nSP_TOK_ARC_TURNS='2'\nSP_TOK_ARC_CTX='25000'\n" >> "$FIXDIR/arc-positive.env"

base_snap_to arc-skew __none__
printf "SP_COLLECTOR_REV='a1b2c3d'\n" >> "$FIXDIR/arc-skew.env"

# ---- Part 4 fixtures: a timed-out probe renders STALE, not FAULT ------------------------
# Moved from test-cockpit-collector-quota.sh (cluster 8, UC-24, docs/test-plan/cockpit-
# observability.md) — this is the same "? not 0"-shaped badge question as Parts 1 and 3,
# just keyed on a timed-out probe rather than an absent key.
STALE_AT=$(( $(date +%s) - 120 ))
printf "SP_AT='%s'\n" "$STALE_AT" > "$FIXDIR/stale-fault.env"
{
    printf "SP_AT='%s'\n" "$STALE_AT"
    printf "_PROBE_STATUS_core='timeout'\n"
    printf "SP_PROBE_KILLED_core='3'\n"
} > "$FIXDIR/stale-timeout.env"

# ---- self-removed fixture: the SELF section (REPEATING/BIRTH/STALL) never renders -------
# Moved from test-cockpit-self.sh (cluster 8, UC-16/21, docs/test-plan/cockpit-
# observability.md): every SELF trip condition set at once proves the section is gone in one
# row, where the original tested "nothing tripped", "repeating only", "stillborn only" and
# "starved only" as four separate, identically-asserting renders.
base_snap_to self-removed __none__
printf "SP_SELF_REPEATING_N='1'\nSP_SELF_REPEATING0='handled 1 stranded item(s) × 3 passes (6m)'\n" >> "$FIXDIR/self-removed.env"
printf "SP_SELF_STILLBORN_W='1'\nSP_SELF_STILLBORN_LAST='3m ago'\n" >> "$FIXDIR/self-removed.env"
printf "SP_SELF_STARVED_W='2'\nSP_SELF_STARVED_LAST='7m ago'\n" >> "$FIXDIR/self-removed.env"

# ---- e2e fixture: what the fixed collector emits when bdjson returns nothing ------------
# Used by "Part 2 summary" below to prove the collector's ? propagates to the pane's ?,
# closing the end-to-end chain — not just the fixture-directory seam this suite drives it
# through.
{
    printf 'SP_AT=%d\n' "$(date +%s)"
    printf 'SP_AEON_N=0\nSP_NEXT_N=0\nSP_INFLOW_N=0\nSP_INFLOW_WIN=60\nSP_INFLOW_DEFECT=0\nSP_INFLOW_KINDS=-\n'
    printf 'SP_AWAITING_N=0\nSP_WAITING=0\nSP_UNANSWERED=0\n'
    printf 'SP_UNSENT=0\nSP_BRANCH_DONE=0\nSP_UNSENT_OLDEST_H=0\nSP_UNADOPTED=0\nSP_ORPHAN_WORK=0\n'
    printf 'SP_CLOSED_24H=0\nSP_OPENED_24H=0\nSP_BEADS_LANDED_24H=0\n'
    printf 'SP_BEADS_SPARK_OPENED=▁▁▁▁▁▁▁▁\nSP_BEADS_SPARK_CLOSED=▁▁▁▁▁▁▁▁\n'
    printf 'SP_BEADS_SPARK_LANDED=▁▁▁▁▁▁▁▁\nSP_CLOSED_KINDS=-\n'
    printf 'SP_CLOSED=?\nSP_LANDED=?\nSP_UNLANDED_N=?\n'
    printf 'SP_QUEUE_DEPTH=?\nSP_QUEUE_EJECTED=0\nSP_QUEUE_RED=0\n'
    printf 'SP_QUEUE_BATCH_PR=0\nSP_QUEUE_BATCH_N=0\nSP_QUEUE_NEXT_N=0\nSP_QUEUE_NEXT_MAX=8\nSP_QUEUE_QUARANTINE_N=0\n'
    printf 'SP_SENTINEL_TIMER=1\nSP_SENTINEL_AGE=30\nSP_OPS_TIMER=1\nSP_OPS_AGE=30\n'
    printf 'SP_AURON_TIMER=1\nSP_AURON_AGE=30\nSP_AURON_FIRING=0\nSP_AURON_KEYS=\n'
    printf 'SP_GATE_N=0\nSP_GATE_LIVE=0\nSP_CAPACITY_PAUSED=0\n'
} | snap_to e2e

# ---- Render every fixture in ONE health.sh process --------------------------------------

RENDER_ALL="$(env -i PATH="$PATH" HOME="$PD/home" TERM=dumb LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_REPO="$PD/repo" \
    SPIRA_RUN="$PD/repo/.runtime/spira" \
    SPIRA_SYSTEMCTL="$PD/bin/mock-systemctl" \
    SPIRA_SNAP_STALE_S=60 \
    bash "$PANE" render-many "$FIXDIR" 0 0 2>/dev/null \
  | sed 's/\x1b\[[?0-9;]*[a-zA-Z]//g')"

# block <fixture-name> -> that fixture's rendered frame, ANSI already stripped above.
block() {
    awk -v want="=== $1 ===" '
        $0 == want { grab=1; next }
        /^=== / && grab { exit }
        grab { print }
    ' <<< "$RENDER_ALL"
}

# ---- Part 1: renderer fault injection ---------------------------------------------------
# For each SP_* count, a fixture omits it and the pane must show ? not 0.
# A POSITIVE CONTROL PRECEDES EACH FAULT: the 'full' fixture proves the field is reachable
# before its absence is asserted.

echo "Part 1: renderer fault injection — absent snapshot key must render ?, not 0"

p="$(block full)"

# SP_NEXT_N — ready-queue count, guarded by unread_row NEXT
if grep -qF 'NEXT' <<< "$p"; then
    ok "SP_NEXT_N positive control: NEXT section renders"
else
    bad "SP_NEXT_N positive control: NEXT section absent from frame (cannot test fault)"
fi
p="$(block omit-SP_NEXT_N)"
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
p="$(block full)"
if grep -qF 'INFLOW' <<< "$p"; then
    ok "SP_INFLOW_N positive control: INFLOW section renders"
else
    bad "SP_INFLOW_N positive control: INFLOW section absent (cannot test fault)"
fi
p="$(block omit-SP_INFLOW_N)"
if grep -q 'INFLOW.*?' <<< "$p"; then
    ok "SP_INFLOW_N: absent key renders '?' (cannot read what is being cut)"
else
    bad "SP_INFLOW_N: absent key did not render '?': $(printf '%s\n' "$p" | grep -i inflow)"
fi

# SP_AWAITING_N — CI gate count, guarded by unread_row CI
p="$(block full)"
if grep -qF ' CI ' <<< "$p"; then
    ok "SP_AWAITING_N positive control: CI section renders"
else
    bad "SP_AWAITING_N positive control: CI section absent (cannot test fault)"
fi
p="$(block omit-SP_AWAITING_N)"
if grep -q ' CI .*?' <<< "$p"; then
    ok "SP_AWAITING_N: absent key renders '?' (cannot read what is parked on CI)"
else
    bad "SP_AWAITING_N: absent key did not render '?': $(printf '%s\n' "$p" | grep ' CI ')"
fi

# SP_QUEUE_DEPTH — queue depth in the 24h worked row and certified count
p="$(block full)"
if grep -qF 'QUEUE' <<< "$p"; then
    ok "SP_QUEUE_DEPTH positive control: QUEUE section renders"
else
    bad "SP_QUEUE_DEPTH positive control: QUEUE section absent (cannot test fault)"
fi
# When SP_QUEUE_DEPTH is absent, the funnel certify/red rows show ? because the
# SP_FUNNEL_* keys are also absent from the fixture — the pane does not render 0.
p="$(block omit-SP_QUEUE_DEPTH)"
if printf '%s\n' "$p" | grep -qE '(certify|red)[[:space:]]+\?'; then
    ok "SP_QUEUE_DEPTH: absent key renders '?' (certify/red rows show ?)"
else
    bad "SP_QUEUE_DEPTH: absent key did not render '?': $(printf '%s\n' "$p" | grep -iE 'queue|certify|red')"
fi

# SP_WAITING — operator attention count, rendered with ${SP_WAITING:-?}
p="$(block full)"
if grep -qF 'ATTN' <<< "$p"; then
    ok "SP_WAITING positive control: ATTN section renders"
else
    bad "SP_WAITING positive control: ATTN section absent (cannot test fault)"
fi
p="$(block omit-SP_WAITING)"
attn_line="$(printf '%s\n' "$p" | grep ' ATTN ')"
if printf '%s\n' "$attn_line" | grep -q 'waiting on you [?]'; then
    ok "SP_WAITING: absent key renders '?'"
else
    bad "SP_WAITING: absent key did not render '?': $attn_line"
fi

# SP_UNANSWERED — thread-reply count, rendered with ${SP_UNANSWERED:-?}
p="$(block full)"
if grep -qF 'threads awaiting' <<< "$p"; then
    ok "SP_UNANSWERED positive control: thread-reply field renders"
else
    bad "SP_UNANSWERED positive control: thread-reply field absent (cannot test fault)"
fi
p="$(block omit-SP_UNANSWERED)"
attn2_line="$(printf '%s\n' "$p" | grep 'threads awaiting')"
if printf '%s\n' "$attn2_line" | grep -q '[?]$\|[?] '; then
    ok "SP_UNANSWERED: absent key renders '?'"
else
    bad "SP_UNANSWERED: absent key did not render '?': $attn2_line"
fi

# SP_MAIL_UNREAD — mailbox unread count, rendered with ${SP_MAIL_UNREAD:-?}. This row used to
# be test-mail-pane.sh's job, checked so weakly (any '?' anywhere in the frame) that it would
# have passed against a MAIL row that never failed at all (cluster 8, docs/test-plan/
# cockpit-observability.md).
p="$(block full)"
if grep -qF ' MAIL ' <<< "$p"; then
    ok "SP_MAIL_UNREAD positive control: MAIL row renders"
else
    bad "SP_MAIL_UNREAD positive control: MAIL row absent (cannot test fault)"
fi
p="$(block omit-SP_MAIL_UNREAD)"
mail_line="$(printf '%s\n' "$p" | grep ' MAIL ')"
if printf '%s\n' "$mail_line" | grep -qF '? cannot read mailbox'; then
    ok "SP_MAIL_UNREAD: absent key renders '? cannot read mailbox'"
else
    bad "SP_MAIL_UNREAD: absent key did not render '? cannot read mailbox': $mail_line"
fi

# SP_UNSENT — unsent branch count, rendered with ${SP_UNSENT:-?}
p="$(block full)"
if grep -qF 'SEND' <<< "$p"; then
    ok "SP_UNSENT positive control: SEND section renders"
else
    bad "SP_UNSENT positive control: SEND section absent (cannot test fault)"
fi
p="$(block omit-SP_UNSENT)"
send_line="$(printf '%s\n' "$p" | grep ' SEND ')"
if printf '%s\n' "$send_line" | grep -q '[?] branches'; then
    ok "SP_UNSENT: absent key renders '?'"
else
    bad "SP_UNSENT: absent key did not render '?': $send_line"
fi

# SP_CLOSED_24H — 24h closed count, rendered with ${SP_CLOSED_24H:-?}
p="$(block full)"
if grep -qF 'BEADS' <<< "$p"; then
    ok "SP_CLOSED_24H positive control: BEADS section renders"
else
    bad "SP_CLOSED_24H positive control: BEADS section absent (cannot test fault)"
fi
p="$(block omit-SP_CLOSED_24H)"
beads_line="$(printf '%s\n' "$p" | grep ' BEADS ')"
if printf '%s\n' "$beads_line" | grep -q 'closed [?]'; then
    ok "SP_CLOSED_24H: absent key renders '?'"
else
    bad "SP_CLOSED_24H: absent key did not render '?': $beads_line"
fi

# SP_OPENED_24H — 24h opened count, rendered with ${SP_OPENED_24H:-?}
p="$(block omit-SP_OPENED_24H)"
beads_line="$(printf '%s\n' "$p" | grep ' BEADS ')"
if printf '%s\n' "$beads_line" | grep -q 'opened [?]'; then
    ok "SP_OPENED_24H: absent key renders '?'"
else
    bad "SP_OPENED_24H: absent key did not render '?': $beads_line"
fi

# SP_BEADS_LANDED_24H — 24h landed count, rendered with ${SP_BEADS_LANDED_24H:-?}
p="$(block full)"
if grep -q 'in 24h' <<< "$p"; then
    ok "SP_BEADS_LANDED_24H positive control: landed line renders"
else
    bad "SP_BEADS_LANDED_24H positive control: landed line absent (cannot test fault)"
fi
p="$(block omit-SP_BEADS_LANDED_24H)"
if grep -q '[?] in 24h' <<< "$p"; then
    ok "SP_BEADS_LANDED_24H: absent key renders '?'"
else
    bad "SP_BEADS_LANDED_24H: absent key did not render '?': $(printf '%s\n' "$p" | grep 'in 24h')"
fi

# SP_LANDED — landed count on the 24h-worked row, rendered with ${SP_LANDED:-?}
p="$(block full)"
if grep -q '24h worked' <<< "$p"; then
    ok "SP_LANDED positive control: 24h worked row renders"
else
    bad "SP_LANDED positive control: 24h worked row absent (cannot test fault)"
fi
p="$(block omit-SP_LANDED)"
worked_line="$(printf '%s\n' "$p" | grep '24h worked')"
if printf '%s\n' "$worked_line" | grep -q '[?] landed'; then
    ok "SP_LANDED: absent key renders '?'"
else
    bad "SP_LANDED: absent key did not render '?': $worked_line"
fi

# SP_QUEUE_DEPTH — queued count on the 24h-worked row, rendered with ${SP_QUEUE_DEPTH:-?}
p="$(block full)"
# Positive control: SP_QUEUE_DEPTH=5 in the fixture → "5 queued" appears.
if grep -q '5 queued' <<< "$p"; then
    ok "SP_QUEUE_DEPTH positive control: '5 queued' renders on 24h worked row"
else
    bad "SP_QUEUE_DEPTH positive control: '5 queued' absent from 24h worked row (cannot test fault)"
fi
p="$(block omit-SP_QUEUE_DEPTH)"
worked_line="$(printf '%s\n' "$p" | grep '24h worked')"
if printf '%s\n' "$worked_line" | grep -q '[?] queued'; then
    ok "SP_QUEUE_DEPTH: absent key renders '? queued'"
else
    bad "SP_QUEUE_DEPTH: absent key did not render '? queued': $worked_line"
fi
if printf '%s\n' "$worked_line" | grep -q '0 queued'; then
    bad "SP_QUEUE_DEPTH: absent key rendered '0 queued' — all-clear from broken probe"
else
    ok "SP_QUEUE_DEPTH: absent key did not render '0 queued'"
fi

# SP_UNLANDED_N — done count on the 24h-worked row, rendered with ${SP_UNLANDED_N:-?}
p="$(block full)"
if grep -q '1 done' <<< "$p"; then
    ok "SP_UNLANDED_N positive control: '1 done' renders on 24h worked row"
else
    bad "SP_UNLANDED_N positive control: '1 done' absent from 24h worked row (cannot test fault)"
fi
p="$(block omit-SP_UNLANDED_N)"
worked_line="$(printf '%s\n' "$p" | grep '24h worked')"
if printf '%s\n' "$worked_line" | grep -q '[?] done'; then
    ok "SP_UNLANDED_N: absent key renders '?'"
else
    bad "SP_UNLANDED_N: absent key did not render '?': $worked_line"
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

# ---- SP_UNLANDED_N: unsent_keys, bdjson exits 0 emits nothing -----------------------
# When bdjson list --status closed exits 0 but emits nothing, closed_pairs is empty.
# An empty response means the probe refused: SP_UNLANDED_N=? not SP_UNLANDED_N=0.
echo
echo "SP_UNLANDED_N — unsent_keys (bdjson exits 0, emits nothing)"
out_unsent_empty="$(run_probe unsent bd-zero-empty)"
if grep -q '^SP_UNLANDED_N=?' <<< "$out_unsent_empty"; then
    ok "SP_UNLANDED_N: bdjson-exits-0-emits-nothing renders SP_UNLANDED_N=?"
else
    bad "SP_UNLANDED_N: bdjson-exits-0-emits-nothing did not render SP_UNLANDED_N=?: $(grep SP_UNLANDED_N <<< "$out_unsent_empty" | head -1)"
fi
if grep -q '^SP_UNLANDED_N=0' <<< "$out_unsent_empty"; then
    bad "SP_UNLANDED_N: bdjson-exits-0-emits-nothing rendered SP_UNLANDED_N=0 — all-clear from broken probe"
else
    ok "SP_UNLANDED_N: bdjson-exits-0-emits-nothing did not render SP_UNLANDED_N=0"
fi

# Also check the other keys emitted by the same failing probe section.
for _k in SP_CLOSED SP_LANDED SP_UNLANDED_N; do
    if grep -q "^${_k}=?" <<< "$out_unsent_empty"; then
        ok "$_k: bdjson-exits-0-emits-nothing renders ${_k}=?"
    else
        bad "$_k: bdjson-exits-0-emits-nothing did not render ${_k}=?: $(grep "^${_k}=" <<< "$out_unsent_empty" | head -1)"
    fi
done

# ---- Part 2 summary: collector output feeds the pane --------------------------------
# Take the collector output that correctly contains SP_UNLANDED_N=? and verify the pane
# renders it as ? rather than 0 — closing the end-to-end chain. The 'e2e' fixture was
# rendered in the same render-many process as Parts 1 and 3, above.
echo
echo "end-to-end: collector ? propagates to pane ?"
e2e_pane="$(block e2e)"
e2e_worked="$(printf '%s\n' "$e2e_pane" | grep '24h worked')"
if printf '%s\n' "$e2e_worked" | grep -q '[?] done'; then
    ok "end-to-end: collector SP_UNLANDED_N=? propagates to pane '? done'"
else
    bad "end-to-end: collector SP_UNLANDED_N=? did not propagate to pane '? done': $e2e_worked"
fi
if printf '%s\n' "$e2e_worked" | grep -q '0 done'; then
    bad "end-to-end: pane rendered '0 done' despite SP_UNLANDED_N=?"
else
    ok "end-to-end: pane did not render '0 done'"
fi

echo
echo "Part 3: collector skew — absent key with SP_COLLECTOR_REV renders 'coll <rev>', not ?"
echo

# POSITIVE CONTROL: SP_TOK_ARC_WIN present → archivist row shows the real value, no skew marker.
p_arc_pos="$(block arc-positive)"
arc_pos_line="$(printf '%s\n' "$p_arc_pos" | grep archivist)"
if [[ "${arc_pos_line:-}" != *"coll "* ]]; then
    ok "collector skew/positive control: archivist row renders normally (no coll marker)"
else
    bad "collector skew/positive control: archivist row shows coll marker when ARC keys present: $arc_pos_line"
fi

# SKEW CASE: absent SP_TOK_ARC_WIN with SP_COLLECTOR_REV set to a known-old rev.
# The row must show 'coll <rev>' instead of '?' so the operator can distinguish a schema
# gap from a probe failure.
p_skew="$(block arc-skew)"
arc_skew_line="$(printf '%s\n' "$p_skew" | grep archivist)"
if [[ "${arc_skew_line:-}" == *"coll a1b2c3d"* ]]; then
    ok "collector skew: archivist row shows 'coll a1b2c3d'"
else
    bad "collector skew: archivist row did not show 'coll a1b2c3d': ${arc_skew_line:-<no archivist line>}"
fi
if [[ "${arc_skew_line:-}" == *"?"* ]]; then
    bad "collector skew: archivist row showed '?' — indistinguishable from probe failure"
else
    ok "collector skew: archivist row did not show '?'"
fi

echo
echo "Part 4: timed-out probe renders STALE, not FAULT (moved from test-cockpit-collector-quota.sh)"
echo

# POSITIVE CONTROL: stale snapshot without probe timeout renders FAULT, not STALE.
p_fault="$(block stale-fault)"
if printf '%s\n' "$p_fault" | grep -q 'FAULT ('; then
    ok "positive control: stale + no timeout renders FAULT"
else
    bad "positive control: stale + no timeout did not render FAULT: $p_fault"
fi
if printf '%s\n' "$p_fault" | grep -q 'STALE'; then
    bad "positive control: STALE present when no probe timeout"
else
    ok "positive control: STALE absent when no probe timeout"
fi

# MAIN CASE: stale snapshot with a timed-out core probe renders a STALE line, not FAULT.
p_stale="$(block stale-timeout)"
if printf '%s\n' "$p_stale" | grep -q 'STALE'; then
    ok "timeout probe: header badge is STALE"
else
    bad "timeout probe: header badge is not STALE: $p_stale"
fi
if printf '%s\n' "$p_stale" | grep -q 'FAULT ('; then
    bad "timeout probe: FAULT badge present"
else
    ok "timeout probe: FAULT badge absent"
fi
if printf '%s\n' "$p_stale" | grep -q 'core: timeout'; then
    ok "timeout probe: STALE line names probe"
else
    bad "timeout probe: STALE line did not name probe: $p_stale"
fi
if printf '%s\n' "$p_stale" | grep -qF '×3'; then
    ok "timeout probe: STALE line shows kill count"
else
    bad "timeout probe: STALE line did not show kill count: $p_stale"
fi

echo
echo "Part 5: SELF section removed — REPEATING/BIRTH/STALL never render (moved from test-cockpit-self.sh)"
echo

p_self="$(block self-removed)"
if printf '%s\n' "$p_self" | grep -q 'REPEATING'; then
    bad "SELF removed: REPEATING row present despite SP_SELF_REPEATING_N=1"
else
    ok "SELF removed: no REPEATING row"
fi
if printf '%s\n' "$p_self" | grep -q 'BIRTH'; then
    bad "SELF removed: BIRTH row present despite SP_SELF_STILLBORN_W=1"
else
    ok "SELF removed: no BIRTH row"
fi
if printf '%s\n' "$p_self" | grep -q 'STALL'; then
    bad "SELF removed: STALL row present despite SP_SELF_STARVED_W=2"
else
    ok "SELF removed: no STALL row"
fi
if printf '%s\n' "$p_self" | grep -q ' SELF '; then
    bad "SELF removed: SELF label present"
else
    ok "SELF removed: no SELF label"
fi

echo
tl_summary
