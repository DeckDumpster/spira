#!/usr/bin/env bash
#
# testlib-fence.sh — refuse a new or changed test suite that rolls its own assertion
# helpers instead of sourcing testlib.sh.
#
#   testlib-fence.sh
#
# WHY. spira-lint's testlib-migrated rule scans the corpus, and certify_suites=off means
# nothing on the author's own path runs a suite; this is the static author-side check, and
# it also catches a bare pass=/fail= counter.
#
# SCOPE: only spira/test-*.sh paths ADDED or MODIFIED in this branch's diff against its
# base — never the whole corpus, so this costs nothing on a branch that adds or touches no
# suite, and fits certify_suites=off (a static fence, not a suite run). Reads
# SPIRA_GATE_FILES (gate.sh's own STATUS<tab>FILE list) when set, else diffs
# SPIRA_GATE_BASE...HEAD itself; with neither, skips — build-fence.sh's own convention for
# "no diff context to judge".
#
# WHAT IT CATCHES: testlib_migration_offender_pattern (testlib-migration-lib.sh) — a
# testlib.sh primitive redefined as a function, or a bare pass=/fail= counter — in a file
# that never sources testlib.sh. A suite that sources testlib.sh and then layers its own
# helper on top (test-reconciler-flow.sh's own lack(), after migration) is not an offense;
# only a file that never reaches testlib.sh at all is.
#
# EXCEPTIONS: testlib_migration_exceptions, kept equal to spira-lint/testlib-migrated-allow.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
cd "$HERE/.." 2>/dev/null || { printf 'testlib-fence: cannot reach the tree holding %s\n' "$0" >&2; exit 1; }
. "$HERE/testlib-migration-lib.sh"

# touched_test_suites reads STATUS<tab>FILE lines (git diff --name-status, gate.sh's own
# shape) or bare FILE lines from stdin, and prints the repo-relative path of every
# spira/test-*.sh that was added (A) or modified (M) — never one merely renamed or deleted,
# which has no new content of its own to carry an offense.
touched_test_suites() {
    local line status f
    while IFS= read -r line || [ -n "$line" ]; do
        [ -n "$line" ] || continue
        case "$line" in
            *"	"*) status="${line%%	*}"; f="${line#*	}" ;;
            *)      status=""; f="$line" ;;
        esac
        case "$f" in spira/test-*.sh) ;; *) continue ;; esac
        # EMPTY STATUS (a bare FILE list, --name-only shape) has no way to tell added from
        # modified apart, so it is included rather than guessed away. A(dded) and M(odified)
        # are the only statuses with new content of this file's own to judge; D(eleted) has
        # none, and a pure rename (R100, no content change) carries none either — both are
        # skipped rather than checked for an offense that, if present, was already there
        # before this branch touched the file.
        case "$status" in ''|A|M) printf '%s\n' "$f" ;; esac
    done
}

changed=""
if [ -f "${SPIRA_GATE_FILES:-}" ]; then
    changed="$(cat "$SPIRA_GATE_FILES")"
elif [ -n "${SPIRA_GATE_BASE:-}" ]; then
    changed="$(git diff --name-status "$SPIRA_GATE_BASE...HEAD" 2>/dev/null)"
else
    printf 'testlib-fence: SKIP no SPIRA_GATE_FILES or SPIRA_GATE_BASE — no diff to check\n' >&2
    exit 0
fi

suites="$(printf '%s\n' "$changed" | touched_test_suites)"
if [ -z "$suites" ]; then
    printf 'testlib-fence: no added or modified spira/test-*.sh in the diff — skipped\n' >&2
    exit 0
fi

exceptions=" $(testlib_migration_exceptions | tr '\n' ' ') "
pattern="$(testlib_migration_offender_pattern)"

bad=0
while IFS= read -r f; do
    [ -n "$f" ] || continue
    [ -f "$f" ] || continue
    case "$exceptions" in *" $(basename "$f") "*) continue ;; esac
    testlib_migration_sources_testlib "$f" && continue
    helper="$(grep -oE "$pattern" "$f" 2>/dev/null | head -1)"
    [ -n "$helper" ] || continue
    bad=1
    printf 'testlib-fence: %s defines its own assertion helper (%s) instead of sourcing testlib.sh\n' \
        "$f" "$helper" >&2
done <<< "$suites"

if [ "$bad" -ne 0 ]; then
    cat >&2 <<'WHY'
testlib-fence: REFUSED — a suite above rolls its own pass/fail bookkeeping instead of
testlib-fence: sourcing spira/testlib.sh. Source it instead:
testlib-fence:
testlib-fence:   HERE="$(cd "$(dirname "$0")" && pwd)"
testlib-fence:   . "$HERE/testlib.sh"
testlib-fence:
testlib-fence: See testlib.sh's own header for the function names it already provides
testlib-fence: (ok/bad/is/want/nowant/wantrc/skip/bail/tl_summary).
WHY
    exit 1
fi

printf 'testlib-fence: no added or modified suite rolls its own assertion helpers\n'
exit 0
