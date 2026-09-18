#!/usr/bin/env python3
"""
strand-classify.py — the classifier behind strand.sh: partition state in, stranded rows out.

Split out of strand.sh so it can be run against fixtures. Every kind below is a distinct
failure mode with a distinct fix, which is the whole complaint against `gt convoy stranded`:
it collapsed three unrelated conditions into one list and named no action for any of them.

  ghost                in_progress, lease expired, and no live aeon holds it
  deferred-unescalated deferred with no escalation label — the state the statute forbids
  starved              claimable work exists and nothing alive is working it
  stale-blocked        every blocker closed, nothing claimable — is_blocked went stale
  blocked-external     blocked by a bead outside the partition; no aeon can ever clear it
  stuck                blocked by work that is itself not moving
  cycle                a dependency ring: permanently unclaimable
  empty                an open epic with no children
  waiting / poisoned   informational: already in the operator's queue, never escalated again

Input is environment, so this composes with shell: BEADS and READY are `bd ... --json`
payloads, HOLDERS is "<bead-id>\t0|1" lines from the /proc liveness check, LIVE is the
number of running aeons, GHOST_GRACE the lease grace window in seconds.

THE TWO PAYLOADS COME AS FILES, named in BEADS_FILE and READY_FILE. The inline form is kept
for fixtures small enough to write by hand, and it is the form that failed in production:
the kernel caps ONE argv or environment string at MAX_ARG_STRLEN (128 KiB, independent of
ARG_MAX), and a partition of a few hundred beads is several times that. execve then refuses
the classifier outright, every pass, and the sentinel's other lines look ordinary while the
stranded-work check produces nothing.

Output is TSV: kind, id, disposition (act|escalate|info), detail, action.
"""
import json, os

# The escalation label is one configured key; a literal here would silently disagree
# with the predicates and the panel the moment an operator changed it.
ASK  = os.environ.get("SPIRA_ASK_LABEL", "needs-operator")  # literal-ok: Python fallback for direct invocation without conf.sh
# Beads CHECK 2 already exempted via check2_protect_waiting; the ghost check must honour
# the same exemption so strand.sh does not reclaim what the sentinel explicitly protected.
SKIP = os.environ.get("SPIRA_RECLAIM_SKIP_LABEL", "spira-waiting-operator")  # literal-ok: Python fallback for direct invocation without conf.sh
from datetime import datetime, timezone

def load(name):
    path = os.environ.get(name + "_FILE")
    try:
        if path:
            with open(path) as fh: raw = fh.read()
        else:
            raw = os.environ.get(name) or "[]"
        d = json.loads(raw or "[]")
    except Exception: return []
    return d if isinstance(d, list) else [d]

beads = load("BEADS")
ready = {b["id"] for b in load("READY")}
holders = {}
for line in (os.environ.get("HOLDERS") or "").splitlines():
    if "\t" in line:
        k, v = line.split("\t", 1); holders[k] = v == "1"
live = int(os.environ.get("LIVE") or 0)
grace = int(os.environ.get("GHOST_GRACE") or 300)
now = datetime.now(timezone.utc).timestamp()

# CAPACITY AWARENESS. When the account is out of capacity, the sentinel correctly
# declines to summon — that is the fix from sp-mdm — and a correct refusal reads as
# starvation from the outside: ready work exists and nothing is working it.
# CAPACITY_PAUSED=1 is set by classify_one() in strand.sh when capacity_paused() returns
# true; injectable so tests can exercise this path without a live capacity.sh call.
# CAPACITY_DETAIL carries the human-readable window from the pause file, so the row
# names the reset time rather than recommending a check that would find nothing wrong.
_cap_paused = os.environ.get("CAPACITY_PAUSED", "0") == "1"
_cap_detail = os.environ.get("CAPACITY_DETAIL", "the account is out of capacity")
# PASS_TRUNCATED=1 means the last sentinel pass logged "not evaluated (pass budget
# exhausted)" for a fayth that works this partition. Ready beads exist but the pass
# simply never reached them — this is not starvation.
_pass_truncated = os.environ.get("PASS_TRUNCATED", "0") == "1"
# POOL AWARENESS. When the operator sets the task pool to zero (SPIRA_MAX_AEONS=0),
# no aeon can ever be summoned for this partition — that is the operator's intent, not a
# fault. POOL_PAUSED=1 is set by classify_one() in strand.sh; injectable so tests can
# exercise this path directly.
_pool_paused = os.environ.get("POOL_PAUSED", "0") == "1"

# FLEET SLOT AWARENESS. A starved partition under a saturated fleet is queue ordering,
# not starvation — every slot is held by another persona doing real work. Escalating it
# pages the operator about correct behaviour: a busy builder holds the only aeon slot
# while the incident queue waits. TOTAL_LIVE (across all personas) and MAX_AEONS
# (SPIRA_MAX_LIVE_AEONS from the caller) let the classifier distinguish the two cases.
# When MAX_AEONS is not configured (0), the distinction cannot be made — escalate as before.
# TOTAL_LIVE and MAX_AEONS are injected by classify_one() in strand.sh; injectable here so
# tests can exercise the saturated-fleet path without running the full shell layer.
_total_live = int(os.environ.get("TOTAL_LIVE") or 0)
_max_aeons  = int(os.environ.get("MAX_AEONS")  or 0)

by_id = {b["id"]: b for b in beads if b.get("id")}
OPEN = lambda b: b.get("status") != "closed"
lab  = lambda b: set(b.get("labels") or [])
rows = []
def row(kind, ident, disp, detail, action):
    rows.append("\t".join([kind, ident, disp, detail.replace("\t", " "), action.replace("\t", " ")]))

def ts(v):
    if not v: return None
    try: return datetime.fromisoformat(v.replace("Z", "+00:00")).timestamp()
    except Exception: return None

# -- ghost: in_progress, lease expired past the grace window, and no live aeon holds it.
# Both halves are required. The lease alone is a heuristic about time; /proc alone races the
# two windows where a bead is legitimately in_progress with no pidfile yet, or no longer.
# EXEMPT: beads carrying ASK (an escalated decision awaiting the operator) or SKIP (explicitly
# protected by check2_protect_waiting because their only open dep is an ask bead) are
# legitimately waiting — reclaiming them re-summons an aeon that immediately re-derives the
# same diagnosis and exits, producing a loop. The sentinel's CHECK 2 already excludes SKIP
# via --exclude-label; the ghost check must honour the same exclusion (sp-2k5a, sp-qsa1).
for b in beads:
    if b.get("status") != "in_progress": continue
    if holders.get(b["id"]): continue
    if ASK in lab(b) or SKIP in lab(b): continue
    exp = ts(b.get("lease_expires_at"))
    if exp is None or now - exp < grace: continue
    mins = int((now - exp) // 60)
    row("ghost", b["id"], "act",
        "in_progress, lease expired %dm ago, holder %s is not running" % (mins, b.get("assignee") or "?"),
        "bd reclaim --id %s" % b["id"])

# -- deferred without an escalation. law-filed-bead-queued-xor-escalated: a bead is worked in
# topological order XOR escalated with a decision only the operator can resolve. Deferred with no
# escalation label is the third state the statute forbids, and it is invisible to every other
# check because a deferred bead is neither ready, nor in progress, nor closed. 130 of them
# once sat parked across three rigs.
#
# EXEMPT: a deferred bead with at least one "blocks" dep on an open or in_progress bead is
# legitimately waiting — the DAG states the reason. Escalating it would assert "nothing in
# the plan below it can move" when a live aeon IS moving the thing that will free it (sp-hg8q).
# Only escalate when all known blockers are closed or the bead has no blocking deps at all.
for b in beads:
    if b.get("status") != "deferred" or ASK in lab(b):
        continue
    live_blockers = [
        d.get("depends_on_id") for d in (b.get("dependencies") or [])
        if d.get("type") == "blocks"
        and d.get("depends_on_id") in by_id
        and OPEN(by_id[d["depends_on_id"]])
    ]
    if live_blockers:
        continue
    row("deferred-unescalated", b["id"], "escalate",
        "deferred but not labelled %s: %s" % (ASK, b.get("title") or ""),
        "bd update %s --status open, or label it %s with the decision" % (b["id"], ASK))

# -- starved: the condition this check is named for. Claimable work exists and nothing alive
# is working it. Aeons at their concurrency cap is NOT starvation — work is moving.
#
# CAPACITY PAUSE IS NOT STARVATION. When the sentinel is correctly refusing to summon
# because the account is out of capacity, both predicates are true: beads are ready,
# no aeon is live. But nothing is wrong — the harness is doing the right thing. A
# 'starved' row in this state recommends checking the sentinel timer, which is healthy,
# and files a needs-ryan escalation for a condition that clears itself. Instead emit a
# distinct 'capacity-paused' info row naming the window; cmd_check() skips info rows.
#
# SATURATED FLEET IS NOT STARVATION. When every aeon slot is held by another partition,
# this partition is waiting in line — a fleet at its cap with active work is doing the
# right thing. Escalating it would page the operator about correct scheduling. Emit a
# 'fleet-saturated' info row (skipped by cmd_check) so the condition remains visible in
# strand.sh report without filing a false alarm.
#
# BOTH PATHS CITE ONLY THIS PARTITION'S BEADS — the detail comes from `ready`, which was
# filtered to this partition's labels by classify_one(). The partition in the title and
# the beads in the body are the same value, read once from the same source.
if ready and live == 0:
    if _cap_paused:
        row("capacity-paused", "-", "info",
            "no aeon summoned: %s — %d bead(s) will be claimed when capacity reopens: %s" % (
                _cap_detail, len(ready), " ".join(sorted(ready)[:6])),
            "none — the sentinel will summon when the account is open again")
    elif _pool_paused:
        row("pool-paused", "-", "info",
            "%d bead(s) ready but the task pool is set to zero — no aeon can be summoned: %s" % (
                len(ready), " ".join(sorted(ready)[:6])),
            "none — raise the task pool (aeons.sh pool <n>) when ready to resume")
    elif _pass_truncated:
        row("pass-truncated", "-", "info",
            "%d bead(s) ready but the last sentinel pass was truncated before evaluating this partition: %s" % (
                len(ready), " ".join(sorted(ready)[:6])),
            "the pass was slow — check bd lock contention; this partition will be evaluated next pass")
    elif _max_aeons > 0 and _total_live >= _max_aeons:
        # Every slot is held by another partition. This is queue ordering, not a fault;
        # do not page the operator. The row is still emitted (as info) so strand.sh report
        # shows the condition without cmd_check acting on it.
        row("fleet-saturated", "-", "info",
            "%d bead(s) ready but all %d aeon slot(s) are held by other partitions (%d live across fleet): %s" % (
                len(ready), _max_aeons, _total_live, " ".join(sorted(ready)[:6])),
            "none — will be claimed when a slot opens")
    else:
        # True starvation: ready work exists, no aeon serves this partition, and the
        # fleet has capacity. The slot detail names the counts so the operator can verify
        # at a glance without checking the fleet panel separately.
        if _max_aeons > 0:
            slot_info = "; 0 of %d aeon slot(s) are serving this partition (%d live across fleet)" % (
                _max_aeons, _total_live)
        else:
            slot_info = ""
        row("starved", "-", "escalate",
            "%d bead(s) ready and no live aeon%s: %s" % (
                len(ready), slot_info, " ".join(sorted(ready)[:6])),
            "check spira-sentinel.timer and the tail of sentinel.log")

# -- per-pilgrimage analysis. An epic whose open children are none of ready, in progress,
# escalated or poisoned is stuck, and the interesting part is WHY.
children = {}
for b in beads:
    if b.get("parent"): children.setdefault(b["parent"], []).append(b)

for e in beads:
    if e.get("issue_type") != "epic" or not OPEN(e): continue
    kids = children.get(e["id"], [])
    if not kids:
        row("empty", e["id"], "escalate", "open epic with no children: %s" % (e.get("title") or ""),
            "decompose it, or close it")
        continue
    live_kids = [c for c in kids if OPEN(c)]
    if not live_kids: continue                      # complete; pilgrimage.sh owns that
    if any(c["id"] in ready or c.get("status") == "in_progress" for c in live_kids):
        continue                                    # moving

    parked  = [c for c in live_kids if ASK in lab(c) or c.get("status") == "deferred"]
    poisonc = [c for c in live_kids if "spira-poison" in lab(c)]
    excused = {c["id"] for c in parked} | {c["id"] for c in poisonc}
    rest    = [c for c in live_kids if c["id"] not in excused]
    if not rest:
        # Not a strand. Waiting on the operator is a queue they already has, and a poisoned bead has
        # already been escalated by the sentinel's poison check. Repeating either here is
        # how a real alert ends up buried under standing ones.
        kind = "waiting" if parked else "poisoned"
        who  = " ".join(c["id"] for c in (parked or poisonc))
        row(kind, e["id"], "info", "%d open child(ren), all %s: %s" %
            (len(live_kids), "awaiting the operator" if parked else "poisoned", who), "none — already queued")
        continue

    unmet, foreign = [], []
    for c in rest:
        for d in c.get("dependencies") or []:
            if d.get("type") != "blocks": continue
            t = d.get("depends_on_id")
            if not t: continue
            dep = by_id.get(t)
            if dep is None: foreign.append((c["id"], t))
            elif OPEN(dep): unmet.append((c["id"], t))

    ids = " ".join(c["id"] for c in rest)
    if not unmet and not foreign:
        # Every blocker is closed, yet nothing is claimable: is_blocked is a cached column and
        # goes stale after an import or a pull. The sentinel recomputes it only when the WHOLE
        # partition is idle, so one stuck epic beside one busy epic is exactly the case that
        # slips through.
        row("stale-blocked", e["id"], "act",
            "%d open child(ren) with every blocker closed, none claimable: %s" % (len(rest), ids),
            "bd recompute-blocked")
    elif foreign:
        row("blocked-external", e["id"], "escalate",
            "blocked by bead(s) outside the partition: %s" %
            " ".join("%s->%s" % p for p in foreign[:5]),
            "close or import the blocker, or drop the edge with bd dep remove")
    else:
        blockers = sorted({t for _, t in unmet})
        movable = [t for t in blockers if t in ready or by_id[t].get("status") == "in_progress"]
        if not movable:
            row("stuck", e["id"], "escalate",
                "%d open child(ren) blocked by work that is itself not moving: %s" %
                (len(rest), " ".join(blockers[:5])),
                "unblock the head of the chain, or re-sequence the plan")

# -- cycles. A dependency cycle is the permanent case of exactly this condition: work exists,
# nothing is ever ready, and no other check can see it because every bead in the ring has a
# perfectly good reason to be blocked. Tarjan over the open sub-graph.
graph, index, low, onstack, stack, comps, counter = {}, {}, {}, set(), [], [], [0]
for b in beads:
    if not OPEN(b): continue
    graph[b["id"]] = [d["depends_on_id"] for d in (b.get("dependencies") or [])
                      if d.get("type") == "blocks" and d.get("depends_on_id") in by_id
                      and OPEN(by_id[d["depends_on_id"]])]
for root in graph:
    if root in index: continue
    work = [(root, iter(graph[root]))]
    index[root] = low[root] = counter[0]; counter[0] += 1
    stack.append(root); onstack.add(root)
    while work:
        v, it = work[-1]
        for w in it:
            if w not in index:
                index[w] = low[w] = counter[0]; counter[0] += 1
                stack.append(w); onstack.add(w)
                work.append((w, iter(graph.get(w, []))))
                break
            elif w in onstack:
                low[v] = min(low[v], index[w])
        else:
            work.pop()
            if work:
                low[work[-1][0]] = min(low[work[-1][0]], low[v])
            if low[v] == index[v]:
                comp = []
                while True:
                    w = stack.pop(); onstack.discard(w); comp.append(w)
                    if w == v: break
                if len(comp) > 1: comps.append(sorted(comp))
for comp in comps:
    row("cycle", comp[0], "escalate",
        "dependency cycle, permanently unclaimable: %s" % " -> ".join(comp),
        "bd dep remove one edge of the ring")

print("\n".join(rows))