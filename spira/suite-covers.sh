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
    # Stop at "set -" so heredocs inside the suite body (fixture suites are common
    # here) cannot spoof the declaration — the same guard suite_exclusive_of and
    # suite_selects_on_of already carry.
    sed -n '/^set -/q;s/^# *requires: *//p' "$1" 2>/dev/null | head -1 | tr ',' ' ' || true
}

suite_testenv_unmet() {  # suite_testenv_unmet <file-path> -> true iff the suite declares
    # `# requires: testenv` and SPIRA_IN_TESTENV is not 1. testenv-batch.sh sets that
    # var on every suite it execs inside the container (sp-nxvjm); a suite reading
    # false here is being run some other way, most often by hand on the host.
    case " $(suite_requires_of "$1") " in
        *" testenv "*) [ "${SPIRA_IN_TESTENV:-}" != 1 ] ;;
        *) return 1 ;;
    esac
}

suite_exclusive_of() {  # suite_exclusive_of <file-path> -> reason string, or empty
    # Empty return means "not exclusive" — the suite runs alongside others in parallel.
    # Non-empty: the reason given after "# exclusive:"; logged when the batch drains
    # in-flight jobs before running this suite alone.
    # Stop at "set -" so heredocs inside the suite body cannot spoof the declaration.
    sed -n '/^set -/q;s/^# *exclusive: *//p' "$1" 2>/dev/null | head -1 || true
}

suite_tier_of() {  # suite_tier_of <file-path> -> the # tier: value (T0..T4), or empty
    # Empty return means undeclared — suites.sh's `list` reports the omission; nothing
    # here treats an empty tier as T-anything.
    sed -n 's/^# *tier: *//p' "$1" 2>/dev/null | head -1 | tr -d '[:space:]' || true
}

suite_uc_of() {  # suite_uc_of <file-path> -> space-separated UC-<area>-NN ids, or empty
    # UC ids share the # covers: line with path globs (testlib.sh's header convention)
    # and are told apart from a glob by the "UC-" prefix, so this filters suite_covers_of's
    # output rather than parsing a second directive that could drift out of step with it.
    local cov
    cov="$(suite_covers_of "$1")"
    [ -n "$cov" ] || return 0
    # || true: grep exits 1 when no token matches "UC-"; under a caller's pipefail that
    # would make this pipeline's status 1 for the ordinary case of no UC ids declared.
    printf '%s\n' "$cov" | tr ' ' '\n' | { grep '^UC-' || true; } | tr '\n' ' ' | sed 's/ $//'
}

suite_selects_on_of() {  # suite_selects_on_of <file-path> -> space-separated event tokens, or empty
    # Tokens are "added" and "mode". A suite with this declaration is selected when a
    # file matching its # covers: glob undergoes one of the listed diff events, instead
    # of (not in addition to) the default content-change trigger.
    # Stop at "set -" so heredocs inside the suite body cannot spoof the declaration.
    # Commas are treated as delimiters so "added,mode" and "added mode" both work.
    sed -n '/^set -/q;s/^# *selects-on: *//p' "$1" 2>/dev/null | head -1 | tr ',' ' ' || true
}

suite_tier_of() {  # suite_tier_of <file-path> -> "T0".."T4", or empty (undeclared)
    # Empty return means no declaration. Unlike # covers:, an empty tier is never
    # a valid "run always" state — test-plan-lint.sh treats it as a missing header.
    # Stop at "set -" so heredocs inside the suite body cannot spoof the declaration.
    sed -n '/^set -/q;s/^# *tier: *//p' "$1" 2>/dev/null | head -1 || true
}

suite_uc_of() {  # suite_uc_of <file-path> -> space-separated UC-<area>-NN tokens from # covers:, or empty
    # UC ids live as tokens on the # covers: line, alongside path globs; this
    # extracts only the tokens shaped like a UC id (path globs never are).
    local _cov _tok _out=""
    _cov="$(suite_covers_of "$1")"
    for _tok in $_cov; do
        case "$_tok" in
            UC-*-[0-9][0-9]) _out="$_out $_tok" ;;
        esac
    done
    printf '%s\n' "${_out# }"
}
