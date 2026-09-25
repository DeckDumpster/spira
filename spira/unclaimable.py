#!/usr/bin/env python3
"""
unclaimable.py — the classifier behind lib.sh's detect_unclaimable_ready.

Split out of lib.sh so it can be driven from a fixture-JSON table instead of a real bd
(same PARTS/ALL_PARTS-env, JSON-on-stdin shape strand-classify.py already uses for the
same reason). Input is the ready set (a bd --json payload, list or single object) on
stdin; PARTS and ALL_PARTS are "<fayth>|<inc-labels-csv>|<exc-labels-csv>" lines, one per
persona — PARTS the active roster, ALL_PARTS the full chamber, so a bead claimable only by
a parked persona reads as waiting rather than unclaimable. Output is one line per bead no
persona can claim: "UNCLAIMABLE <id> — <reason>".
"""
import json, os, sys

try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
beads = d if isinstance(d, list) else [d]

def parse_parts(env_key):
    result = {}
    for line in os.environ.get(env_key, "").splitlines():
        line = line.strip()
        if not line:
            continue
        name, inc_str, exc_str = line.split("|", 2)
        result[name] = (set(filter(None, inc_str.split(","))),
                        set(filter(None, exc_str.split(","))))
    return result

parts = parse_parts("PARTS")
all_parts = parse_parts("ALL_PARTS")

scope_label = os.environ.get("SPIRA_SCOPE_LABEL", "spira")
partition_labels = sorted({lab for inc, _ in all_parts.values() for lab in inc if lab != scope_label})
ci_label = os.environ.get("SPIRA_CI_LABEL", "awaiting-ci")  # literal-ok: Python fallback for direct invocation without conf.sh
ask_label = os.environ.get("SPIRA_ASK_LABEL", "needs-operator")  # literal-ok: Python fallback for direct invocation without conf.sh

for bead in beads:
    L = set(bead.get("labels") or [])
    bid = bead.get("id", "?")
    if L & {ask_label, "spira-poison"}:
        continue
    # A REPORT ABOUT AN UNCLAIMABLE BEAD IS NOT ITSELF A SUBJECT. The report is filed into
    # the incident partition, so whenever that partition is unservable the report is
    # unclaimable too — and reporting it files another report, which is also unclaimable.
    # The dedup key is unclaimable:<subject>, so every link in the chain is a NEW subject
    # and dedup never fires.
    #
    # Measured 2026-09-12: one watchtower incident seeded a 128-deep chain and took the
    # store from 58 beads to 310 in minutes, while the roster was narrowed to a single
    # persona for a focus period. Nothing here was wrong except the missing base case: the
    # detector was correctly reporting a condition it was itself creating more of.
    #
    # external_ref is the structured mark the filer already sets, so this needs no new
    # label and cannot drift from the title text.
    if str(bead.get("external_ref") or "").startswith("unclaimable:"):
        continue
    # CI-parked beads are intentionally excluded from every persona predicate;
    # the exclusion is not a misconfiguration, so they must not appear here.
    if ci_label and ci_label in L:
        continue

    # A bead missing the scope label cannot be claimed by any persona; every predicate
    # requires it. When SPIRA_SCOPE_LABEL is non-empty, READY_ARGS already filters these
    # out at the bd level and this branch is unreachable. It fires only when scope
    # restriction is disabled (SPIRA_SCOPE_LABEL=""), where it correctly names the gap.
    if scope_label not in L:
        print("UNCLAIMABLE %s — missing scope label (%s); "
              "no persona can claim a bead without this label; "
              "add %s or remove from the ready queue" % (bid, scope_label, scope_label))
        continue

    pref = {x.split(":", 1)[1] for x in L if x.startswith("fayth:")}
    claimers = []
    for name, (inc, exc) in parts.items():
        if not inc <= L:
            continue
        if L & exc:
            continue
        if pref and name not in pref:
            continue
        claimers.append(name)

    if claimers:
        continue

    # A bead whose partition belongs to a parked fayth (defined in the chamber but absent
    # from the active roster because SPIRA_FAYTHS was narrowed) is WAITING, not UNCLAIMABLE.
    # The roster is a deliberate operator choice; UNCLAIMABLE is reserved for labels that
    # match no persona in the full chamber — the only case where "add one of: ..." is sound.
    if any(
        inc <= L and not (L & exc) and (not pref or name in pref)
        for name, (inc, exc) in all_parts.items()
        if name not in parts
    ):
        continue

    # Build a diagnostic naming the preference and why each named persona was rejected.
    # Lookup uses all_parts so parked personas are named correctly in the fayth: case.
    if pref:
        reasons = []
        for p in sorted(pref):
            if p not in all_parts:
                reasons.append("%s (not in chamber)" % p)
            elif not all_parts[p][0] <= L:
                missing = sorted(all_parts[p][0] - L)
                reasons.append("%s (partition %s, missing %s)" % (
                    p, sorted(all_parts[p][0]), missing))
            elif L & all_parts[p][1]:
                blocked = sorted(L & all_parts[p][1])
                reasons.append("%s (excluded by own labels %s)" % (p, blocked))
            else:
                reasons.append("%s (unknown reason)" % p)
        pref_str = ", ".join(sorted(pref))
        print("UNCLAIMABLE %s — fayth:%s narrows to %s, but none can claim it: %s; "
              "fix: drop the fayth: label or add the named persona's partition labels" % (
                  bid, pref_str, ", ".join(sorted(pref)), "; ".join(reasons)))
    else:
        print("UNCLAIMABLE %s — spira with no matching partition; "
              "no persona's partition labels are all present; "
              "add one of: %s" % (bid, ", ".join(partition_labels)))
