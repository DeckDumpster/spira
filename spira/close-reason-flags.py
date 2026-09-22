#!/usr/bin/env python3
# close-reason-flags.py — shared statute-phrase check for the close-time fence (aeon.sh)
# and the invalid-close detector (detect_invalid_closed in lib.sh).
#
# Both callers exec() this file so the two cannot disagree about what constitutes an
# unfinished close (law-no-close-reason-admits-unfinished).
#
# Usage: python3 close-reason-flags.py <reason>
# Exit 0 and print admitted phrase; exit 1 if none. A match inside a quoted span
# ("…", '…', `…`) or a parenthetical (…), or on a line that names this file or
# test-ops-closing.sh, is a mention — not an admission.
import re, sys

RED_FLAGS = [
    (r"PERMANENT FIX NEEDED", re.I),
    (r"\bmitigated[- ]only\b", re.I),
    (r"\bTODO\b", 0),
    (r"\btemporar(?:y|ily)\s+(?:fix|workaround|mitigation|patch|hack|solution)", re.I),
    (r"\btemporarily\s+(?:fixed|mitigated|patched|worked around)", re.I),
]

_MENTION_FILES = ("close-reason-flags.py", "test-ops-closing.sh")


def _mask_mentions(line):
    """Replace quoted/parenthetical spans with equal-length spaces."""
    masked = re.sub(r'"[^"]*"', lambda m: ' ' * len(m.group()), line)
    masked = re.sub(r"'[^']*'", lambda m: ' ' * len(m.group()), masked)
    masked = re.sub(r'`[^`]*`', lambda m: ' ' * len(m.group()), masked)
    masked = re.sub(r'\([^)]*\)', lambda m: ' ' * len(m.group()), masked)
    return masked


def check_close_reason(reason):
    """Return the first admitted statute phrase, or None."""
    for line in (reason.splitlines() or [reason]):
        if any(f in line for f in _MENTION_FILES):
            continue
        masked = _mask_mentions(line)
        for f, fl in RED_FLAGS:
            m = re.search(f, masked, fl)
            if m:
                return line[m.start():m.end()]
    return None


if __name__ == "__main__":
    reason = sys.argv[1] if len(sys.argv) > 1 else ""
    hit = check_close_reason(reason)
    if hit:
        print(hit)
        sys.exit(0)
    sys.exit(1)
