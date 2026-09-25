#!/usr/bin/env python3
#
# incident-dedup-decision.py <status: open|closed> <fallback: 0|1> <ref>
#
# Extracted from incident.sh's _dedup_incident (UC-ops-detection-remediation-01), which called
# four copies of this same scan inline. Reads a `bd list --json` array from stdin and prints
# the first bead matching <ref>: "open <id>" or "closed <id> <closed_at>". Prints nothing when
# none matches.
#
# <status> selects which candidates count: open,in_progress for the first pass, closed for the
# second. <fallback>=1 skips any bead already carrying a ref: label — those were already
# checked on the label-keyed pass, so the O(N) fallback scan must not re-match them (the same
# bead would otherwise short-circuit the intended "unlabeled beads only" property).
import json
import sys


def main() -> None:
    status, fallback, ref = sys.argv[1], sys.argv[2] == "1", sys.argv[3]
    try:
        beads = json.load(sys.stdin)
    except Exception:
        return
    for bead in beads:
        if fallback and any(l.startswith("ref:") for l in (bead.get("labels") or [])):
            continue
        if bead.get("external_ref") != ref:
            continue
        if status == "open":
            if bead.get("status") in ("open", "in_progress"):
                print("open", bead["id"])
                return
        else:
            if bead.get("status") == "closed":
                print("closed", bead["id"], bead.get("closed_at") or "")
                return


if __name__ == "__main__":
    main()
