#!/usr/bin/env python3
# groomer-litter-predicate.py — the litter/described-unmapped-repo predicate groomer.sh
# sweep's unmapped-repo branch decides on: a bead with no description is litter (closeable
# mechanically); one with a description is left for the model pass.
#
# Usage: bd show <id> --json | python3 groomer-litter-predicate.py
# Prints two lines on stdout:
#   HAS_CONTENT 0|1
#   META created_at: <sanitized>, assignee: <sanitized>
# A malformed or unreadable payload prints HAS_CONTENT 0 (closeable is the fail-open
# direction only in the sense that sweep already requires unmapped-repo AND no description;
# a payload groomer.sh could not even parse gets treated the same as "no description",
# matching the inline python this replaces) and an empty META line.
import json
import re
import sys


def has_content(bead):
    return 1 if (bead.get("description") or "").strip() else 0


def meta(bead):
    ca = re.sub(r"[^A-Za-z0-9:.T_-]", "_", bead.get("created_at") or "unknown")
    ag = re.sub(r"[^A-Za-z0-9._@-]", "_", bead.get("assignee") or "unknown")
    return "created_at: %s, assignee: %s" % (ca, ag)


def main():
    try:
        d = json.load(sys.stdin)
        d = d if isinstance(d, list) else [d]
        bead = d[0]
    except Exception:
        print("HAS_CONTENT 0")
        print("META ")
        return
    print("HAS_CONTENT %d" % has_content(bead))
    print("META %s" % meta(bead))


if __name__ == "__main__":
    main()
