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
    sed -n 's/^# *covers: *//p' "$1" 2>/dev/null | head -1
}
