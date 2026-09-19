#!/usr/bin/env bash
# trust-show.sh — what is known about one red assertion, for the agent holding it.
#
#   trust-show.sh <suite>[::<assertion>]
#
# PROOF OF CONCEPT for sp-pmv67. Not wired into anything.
#
# Answers, in this order, the three questions that decide what to do with a red:
#   1. Has this ever halted a real defect?  (catches — if >0, it is not a disarming candidate)
#   2. Does its verdict depend on anything but the content under test?  (verdicts by condition)
#   3. Is there any record at all?           (no record is NOT reassurance)
set -uo pipefail
TARGET="${1:?usage: trust-show.sh <suite>[::<assertion>]}"
LEDGER="${SPIRA_TRUST_LEDGER:?SPIRA_TRUST_LEDGER must name the ledger file}"
SUITE="${TARGET%%::*}"
ASSERT=""; case "$TARGET" in *::*) ASSERT="${TARGET#*::}" ;; esac
SUITE_DIR="${SPIRA_TRUST_SUITE_DIR:-$(cd "$(dirname "$0")" && pwd -P)}"

python3 - "$LEDGER" "$SUITE" "$ASSERT" "$SUITE_DIR" <<'PY'
import json, sys, collections, os, time
ledger, suite, assertion, sdir = sys.argv[1:5]

rows = []
if os.path.exists(ledger):
    for line in open(ledger):
        line = line.strip()
        if not line: continue
        try: r = json.loads(line)
        except Exception: continue
        if r.get("suite") != suite: continue
        if assertion and r.get("assert") != assertion: continue
        rows.append(r)

# catches: the scar line is the only record of a caught defect that exists today.
scars = []
p = os.path.join(sdir, suite)
if os.path.exists(p):
    for line in open(p, errors="replace"):
        if line.startswith("# scar:"):
            scars.append(line[len("# scar:"):].strip())

print(f"{suite}" + (f"::{assertion}" if assertion else ""))
print()
print(f"  catches  {len(scars)}" + ("   -- NOT a candidate for automatic disarming" if scars else
      "   -- no recorded catch; that is an ABSENCE OF DATA, not a low score"))
for s in scars:
    print(f"           {s[:150]}")
print()

if not rows:
    print("  record   none. This assertion has never been observed by the harvester.")
    print("           Absence of failure is not evidence of reliability: no verdict here.")
    raise SystemExit(0)

by_cond = collections.defaultdict(collections.Counter)
for r in rows:
    by_cond[r.get("cond", "default")][r.get("v")] += 1

print(f"  record   {len(rows)} observation(s), {min(r['t'] for r in rows)}..{max(r['t'] for r in rows)}")
print()
print("  verdict by condition -- the discriminating question is whether ANY of these disagree:")
for cond, c in sorted(by_cond.items()):
    tot = sum(c.values())
    detail = " ".join(f"{k}={v}" for k, v in sorted(c.items()))
    print(f"    {cond:<24} n={tot:<4} {detail}")

conds_with_fail = {c for c, k in by_cond.items() if k.get("fail") or k.get("no-assertions")}
conds_all_ok    = {c for c, k in by_cond.items() if not (k.get("fail") or k.get("no-assertions"))}
print()
if conds_with_fail and conds_all_ok:
    print("  VERDICT DEPENDS ON THE CONDITION, NOT ONLY ON THE CONTENT.")
    print(f"    red under:   {' '.join(sorted(conds_with_fail))}")
    print(f"    green under: {' '.join(sorted(conds_all_ok))}")
    print("    The difference between those conditions is the defect. It is not in the tree")
    print("    under test, and it is not a reason to switch this assertion off.")
elif conds_with_fail:
    print("  RED UNDER EVERY CONDITION TRIED. Nothing here says flake. Treat as a real failure.")
else:
    print("  GREEN UNDER EVERY CONDITION TRIED.")
PY
