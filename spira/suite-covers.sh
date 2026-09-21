# suite-covers.sh — shared # covers: accessor. Sourced, never executed.
#
# ONE PARSER, ONE PLACE. suites.sh and gate-spira.sh both source this file;
# neither carries its own parser. A second implementation is how the two callers
# drift: gate-spira.sh parsed with grep | sed while suites.sh used sed -n, and
# nothing enforced that they agreed (sp-dt8u).
#
# THE RULE THIS ENCODES. A suite with no # covers: line covers everything and must
# never be skipped for want of a declaration. A malformed covers: line (empty after
# stripping the prefix) is treated the same way — the caller sees empty and must
# interpret that as "run always", never as "skip".
# covers: spira/suites.sh spira/gate-spira.sh

suite_covers_of() {  # suite_covers_of <file-path> -> the # covers: globs, or empty
    # Empty return means "covers everything" — callers must treat it as "run always",
    # never as "no declaration means skip".
    # Malformed (prefix present but empty rest): also returns empty, same rule.
    # || true: sed exits non-zero on SIGPIPE or missing file; callers check output only.
    sed -n 's/^# *covers: *//p' "$1" 2>/dev/null | head -1 || true
}

suite_requires_of() {  # suite_requires_of <file-path> -> space-separated requirement tokens, or empty
    # Commas are treated as delimiters so both "claude, bd" and "claude bd" work.
    # Empty return means no declared requirements — the suite runs unconditionally.
    sed -n 's/^# *requires: *//p' "$1" 2>/dev/null | head -1 | tr ',' ' ' || true
}

suite_exclusive_of() {  # suite_exclusive_of <file-path> -> reason string, or empty
    # Empty return means "not exclusive" — the suite runs alongside others in parallel.
    # Non-empty: the reason given after "# exclusive:"; logged when the batch drains
    # in-flight jobs before running this suite alone.
    # Stop at "set -" so heredocs inside the suite body cannot spoof the declaration.
    sed -n '/^set -/q;s/^# *exclusive: *//p' "$1" 2>/dev/null | head -1 || true
}

suite_selects_on_of() {  # suite_selects_on_of <file-path> -> space-separated event tokens, or empty
    # Tokens are "added" and "mode". A suite with this declaration is selected when a
    # file matching its # covers: glob undergoes one of the listed diff events, instead
    # of (not in addition to) the default content-change trigger.
    # Stop at "set -" so heredocs inside the suite body cannot spoof the declaration.
    # Commas are treated as delimiters so "added,mode" and "added mode" both work.
    sed -n '/^set -/q;s/^# *selects-on: *//p' "$1" 2>/dev/null | head -1 | tr ',' ' ' || true
}
