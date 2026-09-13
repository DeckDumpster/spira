#!/usr/bin/env bash
#
# select.sh — the ONE suite selector.
#
#   select.sh --all
#   select.sh --base <ref> --head <ref>
#
# Prints one suite name (basename only) per line to stdout. Exit 0 with empty
# output means "nothing to run" — not an error.
#
# THE ONE IMPLEMENTATION. gate-spira.sh and testenv-batch.sh both call this;
# neither carries a selection loop of its own. The algorithm lives here once,
# so the two callers cannot disagree about what to run.
#
# THE INDEX IS AN INTERFACE. The mapping suite→paths is read through
# suite_covers_of() in suite-covers.sh. The source can be swapped (a
# trace-built index, a compiled manifest) without touching the gate or the
# runner — a drop-in behind the same function signature is sufficient.
#
# MODES
#   --all                     print every suite in SUITE_DIR
#   --base <ref> --head <ref> diff-derived: run suites whose # covers: globs
#                             intersect the branch diff, plus always-run suites
#                             (those with no # covers: line). An unmapped file
#                             (declared by no suite) triggers the all-suites
#                             fallback (law-absence-needs-a-positive-control).
#   --files <path>            file-list-derived: same algorithm as --base/--head
#                             but the changed-file list is read from <path> (one
#                             path per line) rather than computed via git diff.
#                             gate.sh pre-computes this list; passing it here
#                             avoids computing the diff twice and lets test
#                             fixtures supply the list directly without needing
#                             git refs.
#
# ENVIRONMENT (all optional)
#   SPIRA_BATCH_SUITE_DIR     where to find test-*.sh; default: dir of this script
#   SPIRA_REPO                the git repository to diff; default: parent of SUITE_DIR
#
# OPTIONS
#   --repo <path>             override SPIRA_REPO for this invocation
#   --suite-dir <dir>         override SPIRA_BATCH_SUITE_DIR for this invocation
#   --mode-file <path>        write the selection mode ("diff" or "all") to this file
#                             so callers can record how suites were chosen
#   --no-all-fallback         suppress the all-suites fallback for unmapped files;
#                             unmapped files then contribute nothing beyond the
#                             already-covered and always-run suites (use this for
#                             a fast gate where the timed runner handles thorough
#                             coverage — law-absence-needs-a-positive-control still
#                             governs the timed run)
#
# covers: spira/suite-covers.sh spira/gate-spira.sh spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
[ -r "$HERE/suite-covers.sh" ] || { printf 'select: suite-covers.sh is missing\n' >&2; exit 1; }
. "$HERE/suite-covers.sh"

SUITE_DIR="${SPIRA_BATCH_SUITE_DIR:-$HERE}"
REPO="${SPIRA_REPO:-}"
MODE_FILE=""
_ARG_BASE=""
_ARG_HEAD=""
_ARG_FILES=""
_ARG_ALL=0
_ARG_NO_FALLBACK=0

while [ $# -gt 0 ]; do
    case "$1" in
        --all)
            _ARG_ALL=1; shift ;;
        --base)
            [ $# -ge 2 ] || { printf 'select: --base requires an argument\n' >&2; exit 2; }
            _ARG_BASE="$2"; shift 2 ;;
        --base=*)
            _ARG_BASE="${1#--base=}"; shift ;;
        --head)
            [ $# -ge 2 ] || { printf 'select: --head requires an argument\n' >&2; exit 2; }
            _ARG_HEAD="$2"; shift 2 ;;
        --head=*)
            _ARG_HEAD="${1#--head=}"; shift ;;
        --files)
            [ $# -ge 2 ] || { printf 'select: --files requires an argument\n' >&2; exit 2; }
            _ARG_FILES="$2"; shift 2 ;;
        --files=*)
            _ARG_FILES="${1#--files=}"; shift ;;
        --repo)
            [ $# -ge 2 ] || { printf 'select: --repo requires an argument\n' >&2; exit 2; }
            REPO="$2"; shift 2 ;;
        --repo=*)
            REPO="${1#--repo=}"; shift ;;
        --suite-dir)
            [ $# -ge 2 ] || { printf 'select: --suite-dir requires an argument\n' >&2; exit 2; }
            SUITE_DIR="$2"; shift 2 ;;
        --suite-dir=*)
            SUITE_DIR="${1#--suite-dir=}"; shift ;;
        --mode-file)
            [ $# -ge 2 ] || { printf 'select: --mode-file requires an argument\n' >&2; exit 2; }
            MODE_FILE="$2"; shift 2 ;;
        --mode-file=*)
            MODE_FILE="${1#--mode-file=}"; shift ;;
        --no-all-fallback)
            _ARG_NO_FALLBACK=1; shift ;;
        --)
            shift; break ;;
        -*)
            printf 'select: unknown option: %s\n' "$1" >&2
            printf 'usage: select.sh (--all | --base <ref> --head <ref> | --files <path>) [options]\n' >&2
            exit 2 ;;
        *)
            printf 'select: unexpected argument: %s\n' "$1" >&2
            exit 2 ;;
    esac
done

# Validate mode — exactly one of: --all, --base/--head, --files
if [ "$_ARG_ALL" -eq 1 ]; then
    { [ -z "$_ARG_BASE" ] && [ -z "$_ARG_HEAD" ] && [ -z "$_ARG_FILES" ]; } || {
        printf 'select: --all is mutually exclusive with --base/--head and --files\n' >&2; exit 2
    }
elif [ -n "$_ARG_FILES" ]; then
    { [ -z "$_ARG_BASE" ] && [ -z "$_ARG_HEAD" ]; } || {
        printf 'select: --files is mutually exclusive with --base/--head\n' >&2; exit 2
    }
    [ -r "$_ARG_FILES" ] || { printf 'select: --files: %s: not readable\n' "$_ARG_FILES" >&2; exit 2; }
elif [ -n "$_ARG_BASE" ] && [ -n "$_ARG_HEAD" ]; then
    : # diff mode via git
elif [ -n "$_ARG_BASE" ] || [ -n "$_ARG_HEAD" ]; then
    printf 'select: --base and --head must be given together\n' >&2; exit 2
else
    printf 'usage: select.sh (--all | --base <ref> --head <ref> | --files <path>) [options]\n' >&2; exit 2
fi

# Default REPO to parent of SUITE_DIR when not otherwise set
[ -n "$REPO" ] || REPO="$(cd "$SUITE_DIR/.." && pwd -P 2>/dev/null)" || REPO=""

# Build the suite corpus
_all=""
for _f in "$SUITE_DIR"/test-*.sh; do
    [ -r "$_f" ] || continue
    _all="$_all $(basename "$_f")"
done

_write_mode() {   # _write_mode diff|all
    [ -n "$MODE_FILE" ] && printf '%s\n' "$1" > "$MODE_FILE" || true
}

# --all mode: print every suite
if [ "$_ARG_ALL" -eq 1 ]; then
    _write_mode all
    for _s in $_all; do printf '%s\n' "$_s"; done
    exit 0
fi

# --base/--head or --files mode: build the changed-file list
_cv_changed=""
if [ -n "$_ARG_FILES" ]; then
    # Pre-computed file list — read it directly (avoids git diff and lets test
    # fixtures supply the list without git refs).
    while IFS= read -r _cv_f || [ -n "$_cv_f" ]; do
        [ -n "$_cv_f" ] || continue
        _cv_changed="$_cv_changed $_cv_f"
    done < "$_ARG_FILES"
else
    while IFS= read -r _cv_f || [ -n "$_cv_f" ]; do
        [ -n "$_cv_f" ] || continue
        _cv_changed="$_cv_changed $_cv_f"
    done < <(git -C "$REPO" diff --name-only "${_ARG_BASE}...${_ARG_HEAD}" 2>/dev/null || true)
fi

if [ -z "$_cv_changed" ]; then
    # No changed files: only always-run (no # covers:) suites.
    _write_mode diff
    for _s in $_all; do
        _cov="$(suite_covers_of "$SUITE_DIR/$_s")"
        [ -z "$_cov" ] && printf '%s\n' "$_s"
    done
    exit 0
fi

# Collect always-run (no # covers:) suites.
_cv_nocov=""
for _s in $_all; do
    _cov="$(suite_covers_of "$SUITE_DIR/$_s")"
    [ -z "$_cov" ] && _cv_nocov="$_cv_nocov $_s"
done

# For each changed file, find suites whose # covers: globs match.
_cv_unmapped=""
_cv_selected=""
for _cv_f in $_cv_changed; do
    _cv_hit=0
    set -f
    for _s in $_all; do
        _cov="$(suite_covers_of "$SUITE_DIR/$_s")"
        [ -z "$_cov" ] && continue
        for _cv_pat in $_cov; do
            case "$_cv_f" in
                $_cv_pat)
                    _cv_hit=1
                    case " $_cv_selected " in
                        *" $_s "*) ;;
                        *) _cv_selected="$_cv_selected $_s" ;;
                    esac ;;
            esac
        done
    done
    set +f
    [ "$_cv_hit" -eq 0 ] && _cv_unmapped="$_cv_unmapped $_cv_f"
done

if [ -n "$_cv_unmapped" ] && [ "$_ARG_NO_FALLBACK" -eq 0 ]; then
    # UNMAPPED FILE FALLBACK. At least one changed file is claimed by no suite.
    # Run all suites — absence of a declaration must not read as a pass on the
    # changed file (law-absence-needs-a-positive-control). The selector decides
    # this; the runner does not carry this policy.
    # Suppressed by --no-all-fallback for callers (e.g. the landing gate) that
    # keep the gate cheap: the timed runner without --no-all-fallback handles
    # thorough coverage; the gate runs covered+nocov suites only.
    _write_mode all
    for _s in $_all; do printf '%s\n' "$_s"; done
    exit 0
fi

# Merge coverage-selected suites with always-run (no-covers) suites; deduplicate.
_write_mode diff
_cv_deduped=""
for _s in $_cv_selected $_cv_nocov; do
    case " $_cv_deduped " in
        *" $_s "*) ;;
        *) _cv_deduped="$_cv_deduped $_s" ;;
    esac
done
for _s in $_cv_deduped; do printf '%s\n' "$_s"; done
