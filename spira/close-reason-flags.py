#!/usr/bin/env python3
# close-reason-flags.py — shared statute-phrase check for the close-time fence (aeon.sh)
# and the invalid-close detector (detect_invalid_closed in lib.sh).
#
# Both callers exec() this file so the two cannot disagree about what constitutes an
# unfinished close (law-no-close-reason-admits-unfinished), or, for check_unfiled_follow,
# about what constitutes follow-on work left untracked.
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

# Phrases that imply a follow-on obligation. Only a violation when no tracking reference
# is cited. "workaround" is NOT here — a workaround can be complete, verified and landed;
# this measures the property (remainder exists and has no tracking), not the word.
# "upstream" alone names a destination, not an unfinished remainder, so it is not here either.
FOLLOW_ON = [
    "builders should",
    "at scale",
    "the real fix",
    "follow-up",
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


def detect_close_reason(reason):
    """Return the first RED_FLAG match in the raw reason, or None.

    No quote-masking, no mention-file skip: the detector surfaces all occurrences
    so Maechen can decide admission vs quotation (law-a-pattern-match-is-not-an-identity-check).
    """
    for f, fl in RED_FLAGS:
        m = re.search(f, reason, fl)
        if m:
            return reason[m.start():m.end()]
    return None


def check_unfiled_follow(reason, id_prefix="sp"):
    """Return the first follow-on phrase with no tracking reference, or None.

    A tracking reference is a bead id (<id_prefix>-<id>), an owner/repo#N GitHub
    reference, or an https:// URL — any of these means the follow-on work was handed
    off correctly, not left unfiled.
    """
    tracking_re = re.compile(
        r"(?:\b" + re.escape(id_prefix) + r"-[a-z0-9]+"
        r"|[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*#\d+"
        r"|https?://\S+)",
        re.IGNORECASE)
    follow_hit = next((f for f in FOLLOW_ON if f.lower() in reason.lower()), None)
    if follow_hit and not tracking_re.search(reason):
        return follow_hit
    return None


if __name__ == "__main__":
    reason = sys.argv[1] if len(sys.argv) > 1 else ""
    hit = check_close_reason(reason)
    if hit:
        print(hit)
        sys.exit(0)
    sys.exit(1)
