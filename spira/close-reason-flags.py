#!/usr/bin/env python3
# close-reason-flags.py — shared statute-phrase check for the close-time fence (aeon.sh)
# and the invalid-close detector (detect_invalid_closed in lib.sh).
#
# Both callers exec() this file so the two cannot disagree about what constitutes an
# unfinished close (law-no-close-reason-admits-unfinished).
#
# Usage: python3 close-reason-flags.py <reason>
# Exit 0 and print matched phrase when a statute phrase is found; exit 1 otherwise.
import re, sys

RED_FLAGS = [
    (r"PERMANENT FIX NEEDED", re.I),
    (r"\bmitigated[- ]only\b", re.I),
    (r"\bTODO\b", 0),
    (r"\btemporar(?:y|ily)\s+(?:fix|workaround|mitigation|patch|hack|solution)", re.I),
    (r"\btemporarily\s+(?:fixed|mitigated|patched|worked around)", re.I),
]


def check_close_reason(reason):
    """Return the first matching statute phrase, or None."""
    return next(
        (m.group(0) for m in (re.search(f, reason, fl) for f, fl in RED_FLAGS) if m),
        None,
    )


if __name__ == "__main__":
    reason = sys.argv[1] if len(sys.argv) > 1 else ""
    hit = check_close_reason(reason)
    if hit:
        print(hit)
        sys.exit(0)
    sys.exit(1)
