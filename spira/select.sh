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
#   --report-file <path>      write the placement/coverage report to <path>.
#                             Format: one entry per line, prefixed with type:
#                               unplaced:<file>   changed file matched by no suite
#                               unclaimed:<file>  source file claimed by no suite
#                             File is empty when all changed files are placed and
#                             all source files are claimed.
#   --no-all-fallback         suppress the all-suites fallback for unmapped files;
#                             unmapped files then contribute nothing beyond the
#                             already-covered and always-run suites (use this for
#                             a fast gate where the timed runner handles thorough
#                             coverage — law-absence-needs-a-positive-control still
#                             governs the timed run)
#   --no-nocov                suppress always-run (no # covers: line) suites from
#                             the result — the selection then names only suites
#                             whose # covers: glob actually matched a changed
#                             file. For a caller that wants a small, targeted set
#                             (a carve-out inside a suites-off mode) rather than
#                             the normal "everything with no declared coverage
#                             runs too" behaviour.
#   --tiers <csv>             restrict output to suites whose # tier: is in this
#                             comma-separated list (docs/test-plan/README.md).
#                             A suite with no # tier: declaration is always kept —
#                             the corpus is not yet fully migrated, and absence of
#                             a tier must not read as exclusion. Applies after
#                             every other selection rule, including --all.
#
# FILE BUCKETS (select-globs.sh, overridable via SPIRA_SELECT_INERT / SPIRA_SELECT_SOURCE /
# SPIRA_SELECT_PLUMBING)
#   inert    matches SELECT_INERT    → skipped; selects nothing, no fallback
#   source   matches SELECT_SOURCE   → must be claimed; unclaimed exits 1 and names the file
#   plumbing matches SELECT_PLUMBING → all-suites fallback even when a suite claims it —
#                                     a covers: map can be wrong about which suite actually
#                                     exercises shared build/install/runtime scaffolding
#   unknown  everything else         → all-suites fallback when unclaimed (today's behaviour)
#
# covers: spira/suite-covers.sh spira/select-globs.sh spira/gate-spira.sh spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
[ -r "$HERE/suite-covers.sh" ] || { printf 'select: suite-covers.sh is missing\n' >&2; exit 1; }
. "$HERE/suite-covers.sh"
[ -r "$HERE/select-globs.sh" ] || { printf 'select: select-globs.sh is missing\n' >&2; exit 1; }
. "$HERE/select-globs.sh"

# _fn_changed_in FILE — space-separated shell function names whose bodies contain
# at least one changed line in the BASE...HEAD diff for FILE.
# Requires REPO, _ARG_BASE, and _ARG_HEAD (diff mode only); empty in --files mode.
# Clears cur on ^} to detect code between functions; single-line functions may
# attribute nearby global code to the preceding function — acceptable for the
# common multi-line form.
_fn_changed_in() {
    local _fnc_file="$1"
    [ -n "$REPO" ] || return 0
    [ -n "$_ARG_BASE" ] && [ -n "$_ARG_HEAD" ] || return 0
    local _fnc_ranges
    _fnc_ranges="$(git -C "$REPO" diff --unified=0 "${_ARG_BASE}...${_ARG_HEAD}" -- \
        "$_fnc_file" 2>/dev/null \
        | awk '/^@@/{
            match($0, /\+[0-9]+(,[0-9]+)?/)
            part = substr($0, RSTART+1, RLENGTH-1)
            n = split(part, p, ",")
            start = p[1]+0
            count = (n > 1 ? p[2]+0 : 1)
            if (count == 0) next
            printf "%d %d\n", start, start+count-1
        }')"
    [ -n "$_fnc_ranges" ] || return 0
    git -C "$REPO" show "${_ARG_HEAD}:${_fnc_file}" 2>/dev/null \
        | awk -v rng="$_fnc_ranges" '
            BEGIN {
                n = split(rng, rows, "\n")
                for (i = 1; i <= n; i++) {
                    if (rows[i] == "") continue
                    split(rows[i], p, " ")
                    ++nr; rs[nr] = p[1]+0; re[nr] = p[2]+0
                }
                cur = ""
            }
            /^}/ { cur = "" }
            /^[A-Za-z_][A-Za-z0-9_]*\(\)/ {
                match($0, /^[A-Za-z_][A-Za-z0-9_]*/)
                cur = substr($0, RSTART, RLENGTH)
            }
            {
                for (i = 1; i <= nr; i++) {
                    if (NR >= rs[i] && NR <= re[i] && cur != "" && !(cur in seen)) {
                        seen[cur] = 1; printf "%s ", cur
                    }
                }
            }
        '
}

SUITE_DIR="${SPIRA_BATCH_SUITE_DIR:-$HERE}"
REPO="${SPIRA_REPO:-}"
MODE_FILE=""
REPORT_FILE=""
_ARG_BASE=""
_ARG_HEAD=""
_ARG_FILES=""
_ARG_ALL=0
_ARG_NO_FALLBACK=0
_ARG_NO_NOCOV=0
TIERS=""
_all=""
_cv_unmapped=""
_report_ready=0

# On exit: write unplaced changed files and unclaimed source files to REPORT_FILE.
# Runs only after _all is built (_report_ready=1); no-ops on early exits.
_trap_report() {
    [ -n "$REPORT_FILE" ] || return 0
    [ "$_report_ready" -eq 1 ] || return 0
    > "$REPORT_FILE" 2>/dev/null || return 0
    set -f
    for _rp_f in $_cv_unmapped; do
        printf 'unplaced:%s\n' "$_rp_f" >> "$REPORT_FILE"
    done
    [ -n "$REPO" ] || { set +f; return 0; }
    _rp_allpat=""
    for _rp_s in $_all; do
        _rp_cov="${_COV[$_rp_s]-}"
        [ -z "$_rp_cov" ] && continue
        _rp_allpat="$_rp_allpat $_rp_cov"
    done
    while IFS= read -r _rp_f || [ -n "$_rp_f" ]; do
        [ -n "$_rp_f" ] || continue
        case "$_rp_f" in
            */test-*.sh|test-*.sh) continue ;;
            *.sh|*.py) ;;
            *) continue ;;
        esac
        _rp_hit=0
        for _rp_pat in $_rp_allpat; do
            case "$_rp_f" in
                $_rp_pat) _rp_hit=1; break ;;
            esac
        done
        [ "$_rp_hit" -eq 0 ] && printf 'unclaimed:%s\n' "$_rp_f" >> "$REPORT_FILE"
    done < <(git -C "$REPO" ls-files 2>/dev/null || true)
    set +f
}
trap '_trap_report' EXIT

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
        --report-file)
            [ $# -ge 2 ] || { printf 'select: --report-file requires an argument\n' >&2; exit 2; }
            REPORT_FILE="$2"; shift 2 ;;
        --report-file=*)
            REPORT_FILE="${1#--report-file=}"; shift ;;
        --no-all-fallback)
            _ARG_NO_FALLBACK=1; shift ;;
        --no-nocov)
            _ARG_NO_NOCOV=1; shift ;;
        --tiers)
            [ $# -ge 2 ] || { printf 'select: --tiers requires an argument\n' >&2; exit 2; }
            TIERS="$(printf '%s' "$2" | tr ',' ' ')"; shift 2 ;;
        --tiers=*)
            TIERS="$(printf '%s' "${1#--tiers=}" | tr ',' ' ')"; shift ;;
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
for _f in "$SUITE_DIR"/test-*.sh; do
    [ -r "$_f" ] || continue
    _all="$_all $(basename "$_f")"
done
# Build covers map once — suite_covers_of forks a sed, and calling it inside
# the per-file loop costs (corpus × diff) forks per certification.
declare -A _COV
for _s in $_all; do _COV[$_s]="$(suite_covers_of "$SUITE_DIR/$_s")"; done
_report_ready=1

# _tier_ok <suite> -> 0 if TIERS is unset, the suite has no # tier: declaration
# (not yet migrated — must keep running), or its tier is one of TIERS.
_tier_ok() {
    [ -z "$TIERS" ] && return 0
    local _t _tt
    _t="$(suite_tier_of "$SUITE_DIR/$1")"
    [ -z "$_t" ] && return 0
    for _tt in $TIERS; do [ "$_tt" = "$_t" ] && return 0; done
    return 1
}

_write_mode() {   # _write_mode diff|all
    [ -n "$MODE_FILE" ] && printf '%s\n' "$1" > "$MODE_FILE" || true
}

# --all mode: print every suite
if [ "$_ARG_ALL" -eq 1 ]; then
    _write_mode all
    for _s in $_all; do _tier_ok "$_s" && printf '%s\n' "$_s"; done
    exit 0
fi

# --base/--head or --files mode: build the changed-file list.
# _cv_added: files with diff status A (newly added).
# _cv_mode_changed: files whose file mode changed (--base/--head only, from --raw).
# Lines in _ARG_FILES may be STATUS<tab>FILE (from gate.sh name-status output) or bare
# filenames (plain FILE, backwards-compat — treated as M/unknown, not added or mode).
_cv_changed=""
_cv_added=""
_cv_mode_changed=""
if [ -n "$_ARG_FILES" ]; then
    # Pre-computed file list — read it directly (avoids git diff and lets test
    # fixtures supply the list without git refs).
    while IFS= read -r _cv_ln || [ -n "$_cv_ln" ]; do
        [ -n "$_cv_ln" ] || continue
        case "$_cv_ln" in
            *"	"*)  # STATUS<tab>FILE — from --name-status
                _cv_st="${_cv_ln%%	*}"
                _cv_f="${_cv_ln#*	}"
                ;;
            *)     # bare filename — no status info
                _cv_st="M"
                _cv_f="$_cv_ln"
                ;;
        esac
        _cv_changed="$_cv_changed $_cv_f"
        case "$_cv_st" in A) _cv_added="$_cv_added $_cv_f" ;; esac
    done < "$_ARG_FILES"
else
    # --base/--head: use --name-status for add detection, --raw for mode detection.
    while IFS= read -r _cv_ln || [ -n "$_cv_ln" ]; do
        [ -n "$_cv_ln" ] || continue
        _cv_st="${_cv_ln%%	*}"
        _cv_f="${_cv_ln#*	}"
        _cv_changed="$_cv_changed $_cv_f"
        case "$_cv_st" in A) _cv_added="$_cv_added $_cv_f" ;; esac
    done < <(git -C "$REPO" diff --name-status "${_ARG_BASE}...${_ARG_HEAD}" 2>/dev/null || true)
    # Mode-changed files: raw format ":old-mode new-mode ... status<tab>file"
    # A file is mode-changed when old-mode != new-mode and old-mode != 000000 (not new).
    while IFS= read -r _cv_ln || [ -n "$_cv_ln" ]; do
        [ -n "$_cv_ln" ] || continue
        case "$_cv_ln" in :*) ;; *) continue ;; esac
        _cv_om="${_cv_ln:1:6}"; _cv_nm="${_cv_ln:8:6}"
        [ "$_cv_om" != "000000" ] && [ "$_cv_om" != "$_cv_nm" ] && \
            _cv_mode_changed="$_cv_mode_changed ${_cv_ln##*	}"
    done < <(git -C "$REPO" diff --raw "${_ARG_BASE}...${_ARG_HEAD}" 2>/dev/null || true)
fi

if [ -z "$_cv_changed" ]; then
    # No changed files: only always-run (no # covers:) suites.
    _write_mode diff
    if [ "$_ARG_NO_NOCOV" -eq 0 ]; then
        for _s in $_all; do
            _cov="${_COV[$_s]-}"
            [ -z "$_cov" ] && _tier_ok "$_s" && printf '%s\n' "$_s"
        done
    fi
    exit 0
fi

# Strip inert files — they select nothing and do not trigger the fallback.
_cv_live=""
set -f
for _cv_f in $_cv_changed; do
    _cv_is_inert=0
    for _cv_pat in $SELECT_INERT; do
        case "$_cv_f" in $_cv_pat) _cv_is_inert=1; break ;; esac
    done
    [ "$_cv_is_inert" -eq 0 ] && _cv_live="$_cv_live $_cv_f"
done
set +f
if [ -z "$_cv_live" ]; then
    # All changed files are inert — no suite coverage decisions to make, but
    # always-run (no covers:) suites still run. Same as the empty-diff case.
    _write_mode diff
    if [ "$_ARG_NO_NOCOV" -eq 0 ]; then
        for _s in $_all; do
            _cov="$(suite_covers_of "$SUITE_DIR/$_s")"
            [ -z "$_cov" ] && _tier_ok "$_s" && printf '%s\n' "$_s"
        done
    fi
    exit 0
fi
_cv_changed="$_cv_live"

# Plumbing files force the all-suites fallback regardless of whether some
# suite's covers: line claims them — see select-globs.sh for why.
_cv_plumbing=""
set -f
for _cv_f in $_cv_changed; do
    for _cv_pat in $SELECT_PLUMBING; do
        case "$_cv_f" in $_cv_pat) _cv_plumbing="$_cv_plumbing $_cv_f"; break ;; esac
    done
done
set +f

# Collect always-run (no # covers:) suites.
_cv_nocov=""
for _s in $_all; do
    _cov="${_COV[$_s]-}"
    [ -z "$_cov" ] && _cv_nocov="$_cv_nocov $_s"
done

# Suites with # selects-on: are selected only by the selects-on event loop below,
# not by the normal covers content-match. Precompute the set to avoid re-reading
# each suite's declaration inside the O(files * suites) loop.
_son_suite_list=""
for _s in $_all; do
    _son_pre="$(suite_selects_on_of "$SUITE_DIR/$_s")"
    [ -n "$_son_pre" ] && _son_suite_list="$_son_suite_list $_s"
done

# For each changed file, find suites whose # covers: globs match.
# Patterns of the form file#funcname match only when funcname appears in the
# diff for that file. When a file has only function-level patterns and none of
# the declared functions changed, all those suites run — code outside any
# declared function cannot be narrowed further (law-absence-needs-a-positive-control).
_cv_unmapped=""
_cv_selected=""
for _cv_f in $_cv_changed; do
    _cv_hit=0
    _cv_fn_pairs=""  # "suite:funcname" for function-level patterns on this file
    set -f
    for _s in $_all; do
        _cov="${_COV[$_s]-}"
        [ -z "$_cov" ] && continue
        for _cv_pat in $_cov; do
            case "$_cv_pat" in
                *\#*)
                    _cv_fpat="${_cv_pat%%\#*}"
                    _cv_fname="${_cv_pat#*\#}"
                    case "$_cv_f" in
                        $_cv_fpat)
                            _cv_hit=1
                            _cv_fn_pairs="$_cv_fn_pairs ${_s}:${_cv_fname}" ;;
                    esac ;;
                *)
                    case "$_cv_f" in
                        $_cv_pat)
                            _cv_hit=1
                            # Suites with # selects-on: are NOT selected here; their
                            # covers match claims the file (prevents unmapped fallback)
                            # but selection is deferred to the selects-on event loop.
                            case " $_son_suite_list " in *" $_s "*) ;; *)
                                case " $_cv_selected " in
                                    *" $_s "*) ;;
                                    *) _cv_selected="$_cv_selected $_s" ;;
                                esac ;;
                            esac ;;
                    esac ;;
            esac
        done
    done
    set +f

    if [ -n "$_cv_fn_pairs" ]; then
        _cv_fns="$(_fn_changed_in "$_cv_f")"
        _cv_fn_any=0
        for _cv_sfn in $_cv_fn_pairs; do
            _cv_sfn_s="${_cv_sfn%%:*}"
            _cv_sfn_fn="${_cv_sfn#*:}"
            _cv_fn_match=0
            for _cv_cfn in $_cv_fns; do
                [ "$_cv_cfn" = "$_cv_sfn_fn" ] && { _cv_fn_match=1; break; }
            done
            if [ "$_cv_fn_match" -eq 1 ]; then
                _cv_fn_any=1
                case " $_cv_selected " in
                    *" $_cv_sfn_s "*) ;;
                    *) _cv_selected="$_cv_selected $_cv_sfn_s" ;;
                esac
            fi
        done
        if [ "$_cv_fn_any" -eq 0 ]; then
            for _cv_sfn in $_cv_fn_pairs; do
                _cv_sfn_s="${_cv_sfn%%:*}"
                case " $_cv_selected " in
                    *" $_cv_sfn_s "*) ;;
                    *) _cv_selected="$_cv_selected $_cv_sfn_s" ;;
                esac
            done
        fi
    fi

    [ "$_cv_hit" -eq 0 ] && _cv_unmapped="$_cv_unmapped $_cv_f"
done

# Partition unmapped files: source (must be claimed → error) vs unknown (fallback).
# _cv_unmapped is updated before the potential exit so _trap_report sees only unknowns.
_cv_unclaimed_src=""
_cv_unmapped_unk=""
set -f
for _cv_f in $_cv_unmapped; do
    _cv_is_src=0
    for _cv_pat in $SELECT_SOURCE; do
        case "$_cv_f" in $_cv_pat) _cv_is_src=1; break ;; esac
    done
    if [ "$_cv_is_src" -eq 1 ]; then
        _cv_unclaimed_src="$_cv_unclaimed_src $_cv_f"
    else
        _cv_unmapped_unk="$_cv_unmapped_unk $_cv_f"
    fi
done
set +f
_cv_unmapped="$_cv_unmapped_unk"
if [ -n "$_cv_unclaimed_src" ]; then
    for _cv_f in $_cv_unclaimed_src; do
        printf 'select: unclaimed source file: %s\n' "$_cv_f" >&2
    done
    exit 1
fi

if { [ -n "$_cv_unmapped" ] || [ -n "$_cv_plumbing" ]; } && [ "$_ARG_NO_FALLBACK" -eq 0 ]; then
    # ALL-SUITES FALLBACK. At least one changed file is claimed by no suite, or
    # is plumbing whose claim cannot be trusted (see select-globs.sh). Run all
    # suites — absence or unreliability of a declaration must not read as a
    # pass on the changed file (law-absence-needs-a-positive-control). The
    # selector decides this; the runner does not carry this policy.
    # Suppressed by --no-all-fallback for callers (e.g. the landing gate) that
    # keep the gate cheap: the timed runner without --no-all-fallback handles
    # thorough coverage; the gate runs covered+nocov suites only.
    for _cv_f in $_cv_unmapped; do
        printf 'select: %s → [all: unmapped]\n' "$_cv_f" >&2
    done
    for _cv_f in $_cv_plumbing; do
        printf 'select: %s → [all: plumbing]\n' "$_cv_f" >&2
    done
    _cv_n_all=0; for _s in $_all; do _cv_n_all=$((_cv_n_all+1)); done
    printf 'select: fallback — running all %d suites\n' "$_cv_n_all" >&2
    _write_mode all
    for _s in $_all; do _tier_ok "$_s" && printf '%s\n' "$_s"; done
    exit 0
fi

# Selects-on: suites with # selects-on: are selected when a file matching their
# # covers: glob undergoes a listed diff event (added, mode).  This fires in addition
# to the normal covers matching above — a suite may appear in _cv_selected already.
set -f
for _s in $_all; do
    _son="$(suite_selects_on_of "$SUITE_DIR/$_s")"
    [ -n "$_son" ] || continue
    _cov="$(suite_covers_of "$SUITE_DIR/$_s")"
    [ -n "$_cov" ] || continue
    _son_hit=0
    for _son_ev in $_son; do
        case "$_son_ev" in
            added)
                for _cv_af in $_cv_added; do
                    for _cv_pat in $_cov; do
                        case "$_cv_pat" in *\#*) continue ;; esac
                        case "$_cv_af" in
                            $_cv_pat) _son_hit=1; break 3 ;;
                        esac
                    done
                done ;;
            mode)
                for _cv_mf in $_cv_mode_changed; do
                    for _cv_pat in $_cov; do
                        case "$_cv_pat" in *\#*) continue ;; esac
                        case "$_cv_mf" in
                            $_cv_pat) _son_hit=1; break 3 ;;
                        esac
                    done
                done ;;
        esac
        [ "$_son_hit" -eq 1 ] && break
    done
    if [ "$_son_hit" -eq 1 ]; then
        case " $_cv_selected " in
            *" $_s "*) ;;
            *) _cv_selected="$_cv_selected $_s" ;;
        esac
    fi
done
set +f

# Merge coverage-selected suites with always-run (no-covers) suites; deduplicate.
# --no-nocov drops the always-run half — only suites an actual # covers: match named.
_write_mode diff
_cv_deduped=""
[ "$_ARG_NO_NOCOV" -eq 1 ] && _cv_nocov=""
for _s in $_cv_selected $_cv_nocov; do
    case " $_cv_deduped " in
        *" $_s "*) ;;
        *) _cv_deduped="$_cv_deduped $_s" ;;
    esac
done
_cv_n_sel=0; for _s in $_cv_deduped; do _tier_ok "$_s" && _cv_n_sel=$((_cv_n_sel+1)); done
printf 'select: %d suite(s) selected\n' "$_cv_n_sel" >&2
for _s in $_cv_deduped; do _tier_ok "$_s" && printf '%s\n' "$_s"; done
