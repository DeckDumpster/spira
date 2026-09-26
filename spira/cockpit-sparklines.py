#!/usr/bin/env python3
#
# cockpit-sparklines.py — the BEADS section's opened/closed/landed sparklines.
#
#   bdjson list --all ... | cockpit-sparklines.py <landing.log>
#
# Reads bead JSON from stdin (a list or a single object) and the landing.log path from
# argv[1]. Emits SP_CLOSED_24H, SP_OPENED_24H, SP_CLOSED_KINDS and the three sparklines as
# KEY=value lines. Own file (not a cockpit.sh heredoc) so test-beads-sparklines.sh calls the
# real script directly instead of re-extracting it by indentation.
#
# covers: spira/cockpit.sh
import sys, json, datetime, re, os

try: d = json.load(sys.stdin)
except Exception: raise SystemExit
rows = d if isinstance(d, list) else [d]
cut = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(hours=24)
cut_ts = cut.timestamp()

def when(v):
    try: return datetime.datetime.fromisoformat(str(v).replace("Z", "+00:00"))
    except Exception: return None

closed = [i for i in rows if i.get("status") == "closed" and (when(i.get("closed_at") or i.get("updated_at")) or cut) >= cut]
opened = [i for i in rows if (when(i.get("created_at")) or cut - datetime.timedelta(1)) >= cut]
kinds = {}
for i in closed: kinds[i.get("issue_type") or "task"] = kinds.get(i.get("issue_type") or "task", 0) + 1
print("SP_CLOSED_24H=%d" % len(closed))
print("SP_OPENED_24H=%d" % len(opened))
print("SP_CLOSED_KINDS=%s" % (", ".join("%s %d" % (k, v) for k, v in sorted(kinds.items(), key=lambda x: -x[1])[:4]) or "-"))

# Sparklines: bucket the 24h window into BUCKETS equal intervals and count events per bucket.
# EACH SERIES IS SCALED TO ITSELF: the question is "rising or falling" for that series alone.
# ZERO IS A REAL MEASUREMENT: an interval with no events renders ▁, not dropped. Only an
# unreadable source is absent — that distinction is what law-absence-needs-a-positive-control
# requires. Mirroring spark()'s all-equal rule: all-zero → ▁ flat; all-equal non-zero → ▄ flat.
BUCKETS = 8
bucket_secs = 86400 / BUCKETS
blocks = "▁▂▃▄▅▆▇█"

def spark_str(bkts):
    lo, hi = min(bkts), max(bkts)
    if hi == lo:
        return (blocks[0] if hi == 0 else blocks[3]) * len(bkts)
    return "".join(blocks[min(7, int((v - lo) / (hi - lo) * 7.999))] for v in bkts)

opened_bkts = [0] * BUCKETS
for i in rows:
    t = when(i.get("created_at"))
    if t:
        ts = t.timestamp()
        if ts >= cut_ts:
            opened_bkts[min(BUCKETS - 1, int((ts - cut_ts) / bucket_secs))] += 1

closed_bkts = [0] * BUCKETS
for i in rows:
    if i.get("status") == "closed":
        t = when(i.get("closed_at") or i.get("updated_at"))
        if t:
            ts = t.timestamp()
            if ts >= cut_ts:
                closed_bkts[min(BUCKETS - 1, int((ts - cut_ts) / bucket_secs))] += 1

print("SP_BEADS_SPARK_OPENED=%s" % spark_str(opened_bkts))
print("SP_BEADS_SPARK_CLOSED=%s" % spark_str(closed_bkts))

# Landed sparkline: parse landing.log for "spira: landed" lines within the window.
# A missing or unreadable log renders ?, never 0: the reassuring reading must not be the
# one a broken probe produces (law-absence-needs-a-positive-control).
landing_log = sys.argv[1] if len(sys.argv) > 1 else ""
landed_bkts = [0] * BUCKETS
landed_24h = 0
landed_ok = False
if landing_log and os.path.exists(landing_log):
    landed_ok = True
    try:
        with open(landing_log, errors="replace") as f:
            for line in f:
                if "spira: landed" not in line:
                    continue
                m = re.match(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)", line)
                if not m:
                    continue
                try:
                    ts = datetime.datetime.strptime(
                        m.group(1), "%Y-%m-%dT%H:%M:%SZ"
                    ).replace(tzinfo=datetime.timezone.utc).timestamp()
                    if ts >= cut_ts:
                        landed_24h += 1
                        landed_bkts[min(BUCKETS - 1, int((ts - cut_ts) / bucket_secs))] += 1
                except Exception:
                    pass
    except Exception:
        landed_ok = False

if landed_ok:
    print("SP_BEADS_SPARK_LANDED=%s" % spark_str(landed_bkts))
    print("SP_BEADS_LANDED_24H=%d" % landed_24h)
else:
    print("SP_BEADS_SPARK_LANDED=?")
    print("SP_BEADS_LANDED_24H=?")
