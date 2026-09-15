#!/usr/bin/env bash
#
# orphan-test.sh — refuse a diff that orphans a test suite.
#
#   orphan-test.sh           check the current diff; skip (exit 77) with no diff context
#   orphan-test.sh --help    show this header
#
# THE PROPERTY. A diff "orphans" a test when it removes a token from a non-test source file
# and a test suite that is NOT modified by the same diff still contains that token. The test
# now asserts on a symbol that does not exist in the code it is meant to exercise, and it goes
# red on the next timed run, pointed at whichever file the token used to live in. That file is
# correct. The beads are filed as defects in correct code.
#
# THE TWO VIOLATIONS THIS FENCE WAS BUILT FOR (both live at the time of writing):
#
#   1. A commit deleted the sp-recur-N counter labels thirteen hours after a prior commit
#      shipped them. It updated four suites and missed three — test-incident-dedup.sh,
#      test-incident-recur-cause.sh, test-landing.sh — every one of which declared in its
#      # covers: line a file the commit touched. The timed runner filed all three as bugs
#      against spira/incident.sh, which is correct.
#
#   2. world.sh adopted list-units/list-unit-files, leaving the hand-written systemctl stub in
#      test-work-services-exclusion.sh modelling the old shape. work_services() is correct;
#      4 of 7 assertions fail. Marked KNOWN FAILING in gate-suites — a permanent red.
#
# An orphaned test is a false red on a timer, and it arrives pointed at the wrong file. This
# fence catches it at the branch, where the fix is obvious: update the suite in the same diff.
#
# HOW IT WORKS. Cheap and static. From the diff, collect hyphenated tokens removed from
# non-test source files (the class of token in both violations above). Grep spira/test-*.sh
# for each. Refuse when a hit lands in a suite the diff does not touch.
#
# WHAT IT DOES NOT CHECK. Semantic analysis. A token still named by a test that was moved to
# a different function, or renamed by the diff's added lines, is not caught — a grep over
# removed tokens is enough for the two instances above, and cheap enough to sit on every
# branch. False positives are answered by updating the suite or by the override below.
#
# THE OVERRIDE. Add `# orphan-test-ok: <reason>` to the offending line in the test suite, or
# to the line directly above it. The reason must be non-empty text — it is the point of the
# declaration. A suite that keeps an old token deliberately (for a regression guard) says so,
# and the next reader of a violation message will look there first.
#
# WHEN IT RUNS. Only when SPIRA_GATE_BASE is in the environment, meaning this was invoked
# from gate.sh as part of the landing gate. Without it, the fence exits 77 (skip) — the
# scheduled test runner (gate-spira.sh) has no diff and there is nothing to analyze.
#
# Re-violation of law-prefer-the-real-dependency (rung 3) is what promoted this to rung 4.

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"

OVERRIDE_MARKER="orphan-test-ok"

# Without a base ref there is no diff; skip rather than report a false clean.
# A scheduled run (gate-spira.sh without a branch context) is expected to skip.
if [ -z "${SPIRA_GATE_BASE:-}" ]; then
    printf 'orphan-test: SKIP no SPIRA_GATE_BASE — no diff to check (not running from the landing gate)\n'
    exit 77
fi

# Changed-file list: from the gate's file if available, else computed from git.
# The list decides which test suites are "in the diff" and therefore not orphans.
changed_files=""
if [ -f "${SPIRA_GATE_FILES:-}" ]; then
    changed_files="$(cat "$SPIRA_GATE_FILES")"
else
    changed_files="$(git -C "$ROOT" diff --name-only "$SPIRA_GATE_BASE...HEAD" 2>/dev/null)"
fi

# Extract hyphenated tokens from removed lines in non-test source files.
#
# A "hyphenated token" is a sequence starting with a letter, containing letters, digits,
# underscores and hyphens, with at least one hyphen, length ≥ 5. This is the class that
# appears in label names (sp-recur-N), subcommand names (list-units, list-unit-files) and
# the other identifiers that appear literally in test assertions.
#
# Pure alphanumeric tokens are skipped: they appear too commonly in prose, comments, and
# shell syntax to serve as reliable search keys without a high false-positive rate.
#
# A token is "removed" only when it appears on a - line but NOT on any + line of the same
# diff (for non-test files). A token present on both sides was refactored — the content
# changed but the identifier survived — and the test suites referencing it are not orphaned.
# Without this, renaming a comment or wrapping a dolt call in an if-block flags every token
# on the changed lines even when those same tokens appear on the replacement lines.
extract_tokens() {
    git -C "$ROOT" diff "$SPIRA_GATE_BASE...HEAD" 2>/dev/null \
    | awk '
        # Track the file each hunk belongs to so we can skip test suites.
        /^--- a\// {
            f = substr($0, 7)   # strip the "--- a/" prefix
            in_test = (f ~ /\/test-[^/]+\.sh$/ || f ~ /^test-[^/]+\.sh$/)
            next
        }
        /^--- \/dev\/null/ { in_test = 0; next }
        /^\+\+\+ / { next }
        /^diff --git / { in_test = 0; next }

        # Collect tokens from removed (-) and added (+) lines of non-test files separately.
        /^[-+]/ && !in_test && substr($0, 1, 3) != "---" && substr($0, 1, 3) != "+++" {
            sign = (substr($0, 1, 1) == "-") ? 1 : 0
            line = substr($0, 2)
            while (match(line, /[a-zA-Z][a-zA-Z0-9_-]*/)) {
                tok = substr(line, RSTART, RLENGTH)
                line = substr(line, RSTART + RLENGTH)
                if (length(tok) >= 5 && tok ~ /-/) {
                    if (sign) rem[tok] = 1
                    else      add[tok] = 1
                }
            }
        }
        END {
            for (tok in rem) {
                if (!(tok in add)) print tok
            }
        }
    ' | sort -u
}

# scan_suite <file> <token>
# Print "line:text" for each line in the file that contains the token and is NOT covered by
# an orphan-test-ok annotation on that line or the directly preceding non-empty line.
# Exits 0 either way — the caller decides what to do with the output.
scan_suite() {
    local f="$1" tok="$2"
    awk -v TOK="$tok" -v MARK="$OVERRIDE_MARKER" '
    { L[NR] = $0 }
    END {
        for (i = 1; i <= NR; i++) {
            if (index(L[i], TOK) == 0) continue
            # This line or the line directly above it carries the override marker.
            if (L[i] ~ MARK) continue
            prev = i - 1
            while (prev > 0 && L[prev] ~ /^[[:space:]]*$/) prev--
            if (prev > 0 && L[prev] ~ MARK) continue
            printf "%d:%s\n", i, L[i]
        }
    }
    ' "$f"
}

# Build a set of test suites that are NOT modified by this diff.
# A suite in the diff is itself being updated, so it cannot be an orphan.
is_in_diff() {        # is_in_diff <path> -> 0 if the file is in the changed-file list
    local p="$1"
    # Strip leading ./ for comparison
    p="${p#./}"
    while IFS= read -r cf; do
        cf="${cf#./}"
        [ "$cf" = "$p" ] && return 0
    done <<< "$changed_files"
    return 1
}

shopt -s nullglob
suites=("$ROOT"/spira/test-*.sh)
if [ "${#suites[@]}" -eq 0 ]; then
    printf 'orphan-test: no test suites found under %s/spira — skipping\n' "$ROOT"
    exit 77
fi

tokens="$(extract_tokens)"
if [ -z "$tokens" ]; then
    printf 'orphan-test: clean — no hyphenated tokens removed from non-test files\n'
    exit 0
fi

bad=0
while IFS= read -r tok; do
    [ -n "$tok" ] || continue
    for suite in "${suites[@]}"; do
        rel="${suite#"$ROOT"/}"
        # Skip suites that are themselves modified by this diff.
        is_in_diff "$rel" && continue
        hits="$(scan_suite "$suite" "$tok")"
        [ -n "$hits" ] || continue
        bad=1
        # Report which file lost the token and which suite still asserts on it.
        src="$(git -C "$ROOT" diff "$SPIRA_GATE_BASE...HEAD" -- ':!*/test-*.sh' 2>/dev/null \
            | awk -v TOK="$tok" '
                /^--- a\// { src = substr($0, 7) }
                /^-/ && !($0 ~ /^---/) && index($0, TOK) { print src; exit }
            ')"
        printf 'orphan-test: token removed from %s, still asserted in %s:\n' \
            "${src:-<diff>}" "$rel"
        printf '    token: %s\n' "$tok"
        while IFS=: read -r ln text; do
            printf '    %s:%s: %s\n' "$rel" "$ln" "$(printf '%s' "$text" | sed 's/^[[:space:]]*//')"
        done <<< "$hits"
    done
done <<< "$tokens"

if [ "$bad" = 0 ]; then
    printf 'orphan-test: clean — no test suite asserts on a removed token without being updated\n'
    exit 0
fi

cat >&2 <<'WHY'

REFUSED by orphan-test.sh — a token removed from a source file is still asserted in a test
suite that this diff does not touch.

Update the test suite in the same commit, or add an override annotation:

    # orphan-test-ok: <why this assertion still belongs here>

The annotation must be on the offending line in the test suite or immediately above it. It
must have a non-empty reason — the reason is what makes the declaration meaningful.
WHY
exit 1
