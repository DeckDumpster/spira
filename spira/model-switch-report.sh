#!/usr/bin/env bash
#
# model-switch-report.sh — four figures measuring the 2026-09-08 model switch
#
#   model-switch-report.sh [--since <ISO-timestamp|epoch>] [--until <ISO-timestamp|epoch>] [--window <hours>]
#
# The switch moved Ops from Opus 5 → Sonnet and builders from Opus 5 → Opus 4.6
# (commit e0e48f3, 2026-09-08T02:25:06Z). It also widened the Ops sweep from 10 → 30
# minutes, which cuts Ops run count by two thirds on its own.
#
# FOUR FIGURES:
#
#   1. BEADS LANDED PER WINDOW — the outcome (law-measure-the-outcome). From landing.log.
#   2. $ PER LANDED BEAD — efficiency; a cheaper aeon that needs three attempts is not cheaper.
#   3. ATTEMPTS PER LANDED BEAD — where a weaker model shows up first (as retries).
#   4. SOP YIELD FOR OPS — per run: did the Ops SOP application hold? A yield that falls
#      signals the model change went badly.
#
# WHAT WOULD MAKE THIS A FALSE PASS: the sweep widened 10 → 30 minutes at the same switch,
# so total Ops spend per window falls by construction. Compare PER RUN for Ops; compare
# per-landed-bead for builders.
#
# SOURCES:
#   $SPIRA_RUN/aeon-ledger.log   — per-session cost (wall_s, cost_usd, etc.)
#   $SPIRA_RUN/landing.log       — what actually landed (law-closed-is-not-landed)
#   $SPIRA_RUN/sop/applied.jsonl — SOP application records
#
# The default window starts at the model-change epoch. Pass --since to move it, --window to
# limit how far back to look.
#
# Fields a source cannot supply render ?, never 0 (law-absence-needs-a-positive-control).
#
# covers: spira/model-switch-report.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1090
. "$HERE/conf.sh"

CHANGE_EPOCH=1788834306   # 2026-09-08T02:25:06Z — e0e48f3 "Ops → Sonnet, builders → Opus 4.6"
SINCE_EPOCH=""
UNTIL_EPOCH=""
WINDOW_HOURS=""

while [ $# -gt 0 ]; do
    case "$1" in
        --since)
            shift
            # Accept epoch or ISO timestamp
            if [[ "${1:-}" =~ ^[0-9]+$ ]]; then
                SINCE_EPOCH="$1"
            else
                SINCE_EPOCH="$(date -u -d "${1:-}" +%s 2>/dev/null)" || {
                    echo "model-switch-report: cannot parse --since '${1:-}'" >&2; exit 1; }
            fi
            shift ;;
        --window)
            shift
            WINDOW_HOURS="${1:-}"
            shift ;;
        --until)
            shift
            if [[ "${1:-}" =~ ^[0-9]+$ ]]; then
                UNTIL_EPOCH="${1:-}"
            else
                UNTIL_EPOCH="$(date -u -d "${1:-}" +%s 2>/dev/null)" || {
                    echo "model-switch-report: cannot parse --until '${1:-}'" >&2; exit 1; }
            fi
            shift ;;
        --help|-h)
            sed -n '2,/^set /{ /^#/{ s/^# \?//; p }; /^set /q }' "$0"
            exit 0 ;;
        *) echo "model-switch-report: unknown option '$1'" >&2; exit 1 ;;
    esac
done

# SINCE defaults to the model-change epoch. UNTIL defaults to now.
[ -n "$SINCE_EPOCH" ] || SINCE_EPOCH="$CHANGE_EPOCH"
[ -n "${UNTIL_EPOCH:-}" ]  || UNTIL_EPOCH="$(date +%s)"
if [ -n "$WINDOW_HOURS" ]; then
    WIN_S=$(( ${WINDOW_HOURS%.*} * 3600 ))
    SINCE_EPOCH=$(( UNTIL_EPOCH - WIN_S ))
fi

LEDGER="${SPIRA_RUN:-}/aeon-ledger.log"
LANDING="${SPIRA_RUN:-}/landing.log"
SOP_LEDGER="${SPIRA_SOP_LEDGER:-${SPIRA_RUN:-}/sop/applied.jsonl}"

python3 - "$SINCE_EPOCH" "$UNTIL_EPOCH" "$CHANGE_EPOCH" \
              "$LEDGER" "$LANDING" "$SOP_LEDGER" <<'PY'
import sys, re
from datetime import datetime, timezone

since_ep    = int(sys.argv[1])
until_ep    = int(sys.argv[2])
change_ep   = int(sys.argv[3])
ledger_path = sys.argv[4]
landing_path = sys.argv[5]
sop_path    = sys.argv[6]

def ep(ts):
    """ISO timestamp → epoch seconds, or None."""
    try:
        return int(datetime.strptime(ts, "%Y-%m-%dT%H:%M:%SZ")
                   .replace(tzinfo=timezone.utc).timestamp())
    except Exception:
        return None

def fmt(v, decimals=2):
    """Format a float or '?'; never 0 when unknown."""
    if v is None:
        return "?"
    return f"{v:.{decimals}f}"

def fmt_n(v):
    """Format an int or '?'."""
    if v is None:
        return "?"
    return str(v)

# --------------------------------------------------------------------------
# Read the aeon ledger — every `done` line within [since, until]
# --------------------------------------------------------------------------
# Each `done` line:
#   <ts> done <rune> <bead> rc=N status=S [wall_s=N api_s=N turns=N in_tok=N
#        cache_read_tok=N out_tok=N think_tok=N cost_usd=N.NNNN]
# Fields are present only if the session ran far enough to record them.
# Lines without cost_usd are from before sp-udjt and are kept but with cost=None.
# --------------------------------------------------------------------------

DONE_RE = re.compile(
    r'^(\S+) done (\S+) (\S+) rc=(\S+) status=(\S+)(?:\s+(.*))?$'
)
FIELD_RE = re.compile(r'(\w+)=(\S+)')

aeon_rows = []   # list of dicts: ts, rune, bead, rc, status, wall_s, cost_usd, ...

try:
    with open(ledger_path, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = DONE_RE.match(line.strip())
            if not m:
                continue
            ts_str, rune, bead, rc, status, fields_str = m.groups()
            ts = ep(ts_str)
            if ts is None:
                continue
            row = {"ts": ts, "ts_str": ts_str, "rune": rune, "bead": bead,
                   "rc": rc, "status": status}
            if fields_str:
                for k, v in FIELD_RE.findall(fields_str):
                    row[k] = v if v == "?" else (float(v) if "." in v else
                                                  (int(v) if v.lstrip("-").isdigit() else v))
            aeon_rows.append(row)
except FileNotFoundError:
    pass

# --------------------------------------------------------------------------
# Read landing.log — extract bead id from "spira: landed spira/<bead>" lines
# --------------------------------------------------------------------------
LANDED_RE = re.compile(r'^(\S+) \S+: landed (?:\S+/)?([\w.-]+)$')

landed_beads = {}  # bead_id → landing epoch

try:
    with open(landing_path, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = LANDED_RE.match(line.strip())
            if not m:
                continue
            ts = ep(m.group(1))
            if ts is None:
                continue
            bead = m.group(2)
            # keep the FIRST landing for each bead (they should only land once)
            if bead not in landed_beads:
                landed_beads[bead] = ts
except FileNotFoundError:
    pass

# --------------------------------------------------------------------------
# Read SOP ledger — one JSON object per line
# --------------------------------------------------------------------------
import json

sop_rows = []   # list of dicts: ts, bead, check, held, actor, ...

try:
    with open(sop_path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                r = json.loads(line)
            except Exception:
                continue
            ts = r.get("epoch") or ep(r.get("ts", ""))
            if ts is not None:
                r["_ep"] = int(ts)
                sop_rows.append(r)
except FileNotFoundError:
    pass

# --------------------------------------------------------------------------
# Helpers
# --------------------------------------------------------------------------

def in_window(ts):
    return since_ep <= ts < until_ep

def cost_of(row):
    """Return float cost or None."""
    v = row.get("cost_usd")
    if v is None or v == "?":
        return None
    try:
        return float(v)
    except Exception:
        return None

# --------------------------------------------------------------------------
# METRIC 1 — BEADS LANDED PER WINDOW
# --------------------------------------------------------------------------
# Count beads from landing.log whose landing timestamp falls in [since, until].
# Compare to the before-change window of the same length (if data exists).
# --------------------------------------------------------------------------

window_s = until_ep - since_ep
before_start = since_ep - window_s

landed_after  = [b for b, t in landed_beads.items() if in_window(t)]
landed_before = [b for b, t in landed_beads.items()
                 if before_start <= t < since_ep]

# --------------------------------------------------------------------------
# METRIC 2 & 3 — $ PER LANDED BEAD, ATTEMPTS PER LANDED BEAD
# --------------------------------------------------------------------------
# For each landed bead in the window, sum cost across all its `done` ledger
# entries. An entry is attributed to a bead even if its timestamp falls outside
# the window — the attribution is by bead id, not by session time, so retries
# from before the window are counted against their landed bead.
#
# SUMMED COST: any session without cost data contributes 0 to the sum but is
# NOT excluded from the attempt count.
#
# A bead with cost data on NONE of its sessions renders cost=? (cannot say).
# --------------------------------------------------------------------------

def bead_stats(bead_set, rune_filter=None):
    """Return (costs, attempts) lists for beads in bead_set, one entry per bead."""
    costs = []; attempts = []
    for bead in bead_set:
        sessions = [r for r in aeon_rows
                    if r["bead"] == bead
                    and (rune_filter is None or r["rune"] == rune_filter)]
        n = len(sessions)
        if n == 0:
            # Bead landed but no ledger entry — pre-telemetry landing
            costs.append(None)
            attempts.append(None)
        else:
            costs_known = [c for c in (cost_of(r) for r in sessions) if c is not None]
            attempts.append(n)
            costs.append(sum(costs_known) if costs_known else None)
    return costs, attempts

def mean(lst):
    vals = [v for v in lst if v is not None]
    return sum(vals) / len(vals) if vals else None

def median_n(lst):
    vals = sorted(v for v in lst if v is not None)
    if not vals:
        return None
    m = len(vals) // 2
    return vals[m] if len(vals) % 2 else (vals[m-1] + vals[m]) / 2

costs_after, att_after   = bead_stats(landed_after,  rune_filter="builder")
costs_before, att_before = bead_stats(landed_before, rune_filter="builder")

# --------------------------------------------------------------------------
# METRIC 4 — SOP YIELD FOR OPS
# --------------------------------------------------------------------------
# For each Ops session (awake→done pair in the ledger), find the SOP entries
# written against that bead during that session's time window.
# Yield = (held:yes among check:pass applications) / (check:pass applications).
#
# The sweep cadence changed (10→30 min) in the same commit, so the raw count of
# Ops sessions changes. Report per-run, not per-window total.
#
# Coverage = fraction of Ops sessions that applied at least one SOP (check:pass).
# Effectiveness = of those, fraction with held:yes.
# --------------------------------------------------------------------------

# Build Ops session records: for each "done ops" in the ledger, find its "awake ops"
# to establish the session window.
awake_ops = {}  # pid → {ts, bead} -- but we only have the bead for done lines too
# Actually: sequence of awake and done for ops
# awake ops <bead> at ts_a → done ops <bead> at ts_d
# We pair them by bead, picking the closest awake before each done.

# Collect awake ops entries from the raw ledger (lines not covered by DONE_RE)
AWAKE_RE = re.compile(r'^(\S+) awake ops (\S+)$')
awake_rows = []

try:
    with open(ledger_path, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = AWAKE_RE.match(line.strip())
            if not m:
                continue
            ts = ep(m.group(1))
            if ts is not None:
                awake_rows.append({"ts": ts, "bead": m.group(2)})
except FileNotFoundError:
    pass

def ops_sessions_in(start_ep, end_ep):
    """Yield (awake_ts, done_ts, bead) for ops sessions finishing in [start, end)."""
    done_ops = [r for r in aeon_rows
                if r["rune"] == "ops" and start_ep <= r["ts"] < end_ep]
    awake_idx = 0
    for done in sorted(done_ops, key=lambda r: r["ts"]):
        bead = done["bead"]
        done_ts = done["ts"]
        # Find the most recent awake ops entry before done_ts with the same bead.
        # (idle ops sessions have bead="idle"; skip those from done perspective)
        awake_ts = None
        for aw in reversed(awake_rows):
            if aw["ts"] < done_ts and aw["bead"] == bead:
                awake_ts = aw["ts"]
                break
        # If no awake entry found, use done_ts - 3600 as a conservative start
        if awake_ts is None:
            awake_ts = done_ts - 3600
        yield awake_ts, done_ts, bead

def sop_yield(start_ep, end_ep):
    """
    Returns (sessions_with_sop, sessions_total, held_yes, pass_total)
    for Ops sessions in [start_ep, end_ep).
    """
    sessions_total = 0; sessions_with_sop = 0
    held_yes = 0; pass_total = 0

    for aw_ts, done_ts, bead in ops_sessions_in(start_ep, end_ep):
        sessions_total += 1
        # Find SOP applications for this bead during this session window
        apps = [r for r in sop_rows
                if r.get("bead") == bead
                and aw_ts <= r["_ep"] <= done_ts + 60]   # +60s grace for ledger ordering
        passed = [r for r in apps if r.get("check") == "pass"]
        held = [r for r in passed if r.get("held") == "yes"]
        if passed:
            sessions_with_sop += 1
            pass_total += len(passed)
            held_yes += len(held)

    return sessions_with_sop, sessions_total, held_yes, pass_total

sop_wit_after, sop_tot_after, held_yes_after, pass_tot_after = \
    sop_yield(since_ep, until_ep)
sop_wit_before, sop_tot_before, held_yes_before, pass_tot_before = \
    sop_yield(before_start, since_ep)

# --------------------------------------------------------------------------
# Ops per-run cost — corrected for cadence change
# --------------------------------------------------------------------------
def ops_run_costs(start_ep, end_ep):
    return [cost_of(r) for r in aeon_rows
            if r["rune"] == "ops"
            and r.get("status") not in ("idle", "capacity", "paused")
            and start_ep <= r["ts"] < end_ep]

ops_costs_after  = ops_run_costs(since_ep, until_ep)
ops_costs_before = ops_run_costs(before_start, since_ep)

# --------------------------------------------------------------------------
# Render
# --------------------------------------------------------------------------
def epoch_str(ep_s):
    return datetime.fromtimestamp(ep_s, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

since_str = epoch_str(since_ep)
until_str = epoch_str(until_ep)
bef_str   = epoch_str(before_start)
w_h = (until_ep - since_ep) / 3600

print(f"model-switch-report  [{since_str} .. {until_str}]  ({w_h:.1f}h window)")
print(f"before window        [{bef_str} .. {since_str}]  (same length)")
print()

# ---- 1. Beads landed per window ----------------------------------------
n_after  = len(landed_after)
n_before = len(landed_before)
rate_a = f"{n_after/w_h:.1f}/h" if w_h > 0 else "?"
rate_b = f"{n_before/w_h:.1f}/h" if w_h > 0 else "?"
pct = f"{((n_after - n_before)/n_before*100):+.0f}%" if n_before else "n/a"
print(f"1. BEADS LANDED PER WINDOW")
print(f"   after change:   {n_after:4d}  ({rate_a})")
print(f"   before change:  {n_before:4d}  ({rate_b})   delta {pct}")
print()

# ---- 2. $ per landed bead (builders) -----------------------------------
mean_ca = mean(costs_after)
mean_cb = mean(costs_before)
known_a = len([c for c in costs_after  if c is not None])
known_b = len([c for c in costs_before if c is not None])
pct_c = (f"{((mean_ca - mean_cb)/mean_cb*100):+.0f}%"
         if mean_ca is not None and mean_cb and mean_cb != 0 else "n/a")
print(f"2. $ PER LANDED BEAD  (builders; attempts summed per bead)")
print(f"   after change:   ${fmt(mean_ca)} mean  (n={known_a} beads with cost data)")
print(f"   before change:  ${fmt(mean_cb)} mean  (n={known_b} beads with cost data)   delta {pct_c}")
print()

# ---- 3. Attempts per landed bead + poison proxy ------------------------
mean_aa = mean(att_after)
mean_ab = mean(att_before)
multi_a = len([a for a in att_after  if a is not None and a > 1])
multi_b = len([a for a in att_before if a is not None and a > 1])
n_at_a  = len([a for a in att_after  if a is not None])
n_at_b  = len([a for a in att_before if a is not None])
poison_a = f"{multi_a}/{n_at_a}" if n_at_a else "?"
poison_b = f"{multi_b}/{n_at_b}" if n_at_b else "?"
print(f"3. ATTEMPTS PER LANDED BEAD  (builders; >1 = retried)")
print(f"   after change:   {fmt(mean_aa)} mean attempts  {poison_a} needed >1 attempt")
print(f"   before change:  {fmt(mean_ab)} mean attempts  {poison_b} needed >1 attempt")
print()

# ---- 4. SOP yield for Ops (per run) ------------------------------------
eff_a = (held_yes_after  / pass_tot_after  if pass_tot_after  else None)
eff_b = (held_yes_before / pass_tot_before if pass_tot_before else None)
cov_a = (sop_wit_after  / sop_tot_after  if sop_tot_after  else None)
cov_b = (sop_wit_before / sop_tot_before if sop_tot_before else None)
mean_op_a = mean(ops_costs_after)
mean_op_b = mean(ops_costs_before)
print(f"4. SOP YIELD FOR OPS  (per run; corrected for cadence change)")
print(f"   after change:   {sop_tot_after} Ops sessions, {sop_wit_after} applied a SOP")
print(f"     coverage:      {fmt(cov_a) if cov_a is not None else '?'} (fraction of sessions with a passing SOP)")
print(f"     effectiveness: {fmt(eff_a) if eff_a is not None else '?'} (held:yes / check:pass applications, n={pass_tot_after})")
print(f"     cost/run:      ${fmt(mean_op_a)} mean  (n={len([c for c in ops_costs_after if c is not None])} sessions with cost data)")
print(f"   before change:  {sop_tot_before} Ops sessions, {sop_wit_before} applied a SOP")
print(f"     coverage:      {fmt(cov_b) if cov_b is not None else '?'}")
print(f"     effectiveness: {fmt(eff_b) if eff_b is not None else '?'} (n={pass_tot_before})")
print(f"     cost/run:      ${fmt(mean_op_b)} mean  (n={len([c for c in ops_costs_before if c is not None])} sessions with cost data)")
print()

# ---- note on cadence change -------------------------------------------
print("NOTE: sweep widened 10→30 min in the same commit. Total Ops sessions per window")
print("      falls by two thirds — compare per-run figures, not per-window totals.")
PY
