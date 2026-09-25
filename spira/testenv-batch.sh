#!/usr/bin/env bash
# testenv-batch.sh — select, up, install, run, collect, down.
#
# Given a branch: resolves the base ref via spira_landref, computes the full diff
# against it (the release unit), selects suites whose # covers: globs intersect,
# stands up one container, installs the candidate, runs the selected suites,
# collects results onto the host, and tears down.
#
# BASE REF IS RESOLVED, NEVER ASSUMED. Three repos use master; the assumption was
# fixed four times before it held. A wrong base selects nothing, which reads as
# "no suites affected" rather than as a fault — so the suite that proves the
# master-base fixture case is an acceptance criterion, not an afterthought.
#
# RESULT PROTOCOL. Each suite writes <results>/<suite>.result and <results>/<suite>.out.
# Result format: <status> <epoch> <seconds> <fingerprint> <mode> <producer> <rc>.
# A selected suite with no result file is unreached, never green. unreached never
# overwrites a completed status (law-absence-needs-a-positive-control; sp-u1g would
# have overwritten here — that defect is why this protocol exists).
# The mode (parallel or serial) is the 5th field: a green under serial is a weaker
# claim than a green under parallel; the record must not conflate them.
# The producer is the 6th field — who decided which suites to run:
#   explicit  a person or aeon named the suites via --suites
#   diff      the selector derived them from the branch diff
#   all       the diff had an unmapped file; the whole corpus ran as a fallback
# A diff result is a weaker claim than an all result, and an explicit result is
# a weaker claim than either; the record must name what asked for the run.
#
# USAGE
#   testenv-batch.sh [--mode parallel|serial] [--suites <suite1.sh,suite2.sh,...|->]
#                    [--with-bins] [--report [N]]
#                    <branch> [<repo-name-or-path>]
#
#   --suites -  reads suite names from stdin, one per line (blank lines ignored).
#               Empty stdin means "nothing to run" — exit 0, not an error.
#               Composes with a selector:
#                 select.sh --base X --head Y | testenv-batch.sh --suites - <branch> [<repo>]
#   --with-bins builds the branch's own Rust workspace (make build) and copies every
#               executable under target/release/ into the branch worktree's bin/,
#               mirroring the gate's build job exactly (.github/workflows/gate.yml)
#               so a suite that reads shipped binaries (e.g. literal-lint's
#               shipped-mirror scan) sees locally what CI's shipped tree would ship
#               for THIS branch. Without this flag, only $REPO/bin (if present) is
#               copied in — the pre-sp-hr5kj behavior, which never reflects Rust
#               changes made on the branch itself.
#   --report N  print each suite's median wall_secs over its last N run/tsd/ rows (default
#               20), local and CI counted together; no branch or container is required.
#               Delegates to tsd-query.sh suite-medians.
#
# EXIT STATUS
#   0   all selected suites passed or skipped
#   1   suites ran, some were red
#   2   container did not come up, or died mid-batch (harness fault — not the branch)
#   3   install inside the container failed (harness fault — not the branch)
#   4   --with-bins: the candidate's workspace failed to build (branch fault — not harness)
#
# ENVIRONMENT (all optional)
#   SPIRA_BATCH_RESULTS     host root for result directories
#                           (default: SPIRA_RUN/batch-results)
#   SPIRA_BATCH_BINS_TARGET_DIR  CARGO_TARGET_DIR used by --with-bins (default:
#                           SPIRA_RUN/cargo-target-bins). Persistent across runs so
#                           repeat builds are incremental; the branch worktree's own
#                           target/ is not used, so nothing here survives worktree cleanup.
#   SPIRA_BATCH_INSTANCE    container instance name; determines CNAME and the
#                           install instance; default: first 12 chars of the batch key
#   SPIRA_BATCH_SUITE_DIR   where to look for test-*.sh on the host
#                           (default: the spira/ directory beside this script)
#   SPIRA_BATCH_SKIP_INSTALL  if non-empty, skip configure+install; suites that
#                             need installed units will skip (exit 77)
#   SPIRA_BATCH_TIERS       comma-separated # tier: values to run here; default
#                           "T2,T3" — batch and main CI run the integration and
#                           cross-component tiers (docs/test-plan/README.md); T0/T1
#                           already ran at certification. A suite with no # tier:
#                           declaration always runs, so this is a no-op until a
#                           suite carries the header. Only applies to diff-derived
#                           selection; --suites names an explicit list unaffected
#                           by tier.
#   SPIRA_VERDICTS          verdict-cache directory (shared with gate.sh)
#   SPIRA_VERDICT_TTL       cache TTL in seconds; 0 = disabled (default: 86400)
#   SPIRA_VERDICT_REPEAT_CONSIDERED  override when a prior verdict (green or red)
#                                    already exists for this key. Must be a sentence
#                                    of at least 10 characters — a bare flag is refused.
#                                    Use when the prior result was wrong for a cause
#                                    outside the key (e.g., runner destroyed). Recorded
#                                    in the verdict file so a later reader can weigh it.
#   SPIRA_SUITE_TIMEOUT     per-suite wall-clock limit in seconds; 0 = disabled
#                           (default: 600). A suite that exceeds this limit is
#                           recorded as "timeout" and the corpus continues. This
#                           mirrors gate-spira.sh's per-suite watchdog so neither
#                           runner can be held indefinitely by one runaway suite.
#   SPIRA_BATCH_MAXPAR      max parallel suites in --mode parallel. When unset (the normal
#                           case), derived from the guest's own hardware at run time:
#                           min(nproc, floor((MemAvailable - reserve) / per_suite)).
#                           Memory is the binding resource in the guest: each parallel suite
#                           runs inside the batch container, and a guest that exhausts RAM
#                           dies with no annotation. The PID budget (container pids-limit
#                           8192) is a ceiling, not the sizing input. Set to 0 for unlimited.
#   SPIRA_BATCH_MEM_RESERVE_MIB  MiB to hold back from the maxpar formula (default 1024).
#   SPIRA_BATCH_MEM_PER_SUITE_MIB  per-suite memory budget in MiB (default 192; measured
#                           cgroup peak was ~91 MiB across five full-corpus gate runs —
#                           192 is 2x peak). Calibrate from the "cgroup peak" line.
#   SPIRA_BATCH_MEM_AVAIL_MIB  override the MemAvailable reading (testing/debugging only).
#   SPIRA_BATCH_PSI_THRESHOLD  memory PSI avg10 above which suite launches pause (default
#                           10, 0 = disabled). This is the guard the repo variable stood in
#                           for: it slows admission when the guest is already under pressure
#                           rather than preventing it by capping the pool statically.
#   SPIRA_BATCH_ORPHAN_MIN_AGE  seconds a spira-batch-* container with no owner file must
#                           have been running before the orphan sweep reaps it (default
#                           3600). Guards a container mid-startup, whose owner file has not
#                           been written yet, from being swept as if it were abandoned.

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"
. "$HERE/suite-covers.sh"
. "$HERE/suite-state.sh"
. "$HERE/batch-owner.sh"
TESTENV="$HERE/testenv.sh"

# ---------------------------------------------------------------------------
# CONSTANTS — mirror testenv.sh; must agree with the Containerfile values.
# ---------------------------------------------------------------------------
_SPIRA_USER="spirauser"
_SPIRA_UID=1001
_USER_RUNTIME="/run/user/${_SPIRA_UID}"
_CONTAINER_CARGO="/var/spira/cargo"
_CONTAINER_CARGO_TARGET="/var/spira/cargo/target"
_CONTAINER_WORKSPACE="/workspace"

# ---------------------------------------------------------------------------
# ARGS — parse flags before positional arguments.
# ---------------------------------------------------------------------------
MODE="parallel"  # default: parallel is safer and the normal operating mode
SUITES_EXPLICIT=""  # empty: use diff-derived selection; non-empty: use this comma-list
WITH_BINS=0  # --with-bins: build the branch's own workspace binaries into its bin/
_BATCH_REPORT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --mode)
            [ $# -ge 2 ] || { printf 'batch: --mode requires an argument\n' >&2; exit 2; }
            MODE="$2"; shift 2 ;;
        --mode=*)
            MODE="${1#--mode=}"; shift ;;
        --with-bins)
            WITH_BINS=1; shift ;;
        --suites)
            [ $# -ge 2 ] || { printf 'batch: --suites requires an argument\n' >&2; exit 2; }
            [ -z "$SUITES_EXPLICIT" ] || {
                printf 'batch: --suites may only be given once\n' >&2; exit 2
            }
            SUITES_EXPLICIT="$2"; shift 2 ;;
        --suites=*)
            [ -z "$SUITES_EXPLICIT" ] || {
                printf 'batch: --suites may only be given once\n' >&2; exit 2
            }
            SUITES_EXPLICIT="${1#--suites=}"; shift ;;
        --report)
            _BATCH_REPORT="${2:-20}"
            case "$_BATCH_REPORT" in [0-9]*) shift 2 ;; *) _BATCH_REPORT=20; shift ;; esac ;;
        --report=*)
            _BATCH_REPORT="${1#--report=}"; shift ;;
        --)
            shift; break ;;
        -*)
            printf 'batch: unknown option: %s\n' "$1" >&2
            printf 'usage: testenv-batch.sh [--mode parallel|serial] [--suites <list|->] [--with-bins] [--report [N]] <branch> [<repo-name>]\n' >&2
            exit 2 ;;
        *)  break ;;
    esac
done

# --report: delegate to tsd-query.sh and exit; no branch or container needed.
if [ -n "$_BATCH_REPORT" ]; then
    exec bash "$HERE/tsd-query.sh" suite-medians "$_BATCH_REPORT"
fi
case "$MODE" in
    parallel|serial) ;;
    *) printf 'batch: --mode must be parallel or serial, got: %s\n' "$MODE" >&2; exit 2 ;;
esac

BR="${1:-}"
REPO_ARG="${2:-}"
[ -n "$BR" ] || {
    printf 'usage: testenv-batch.sh [--mode parallel|serial] [--with-bins] <branch> [<repo-name>]\n' >&2
    exit 2
}

# ---------------------------------------------------------------------------
# REPO — resolve path and name, following gate.sh's pattern.
# REPO_ARG overrides SPIRA_REPO when provided. lib.sh sources conf.sh which
# sets SPIRA_REPO to the harness directory; a caller supplying a different
# repo path (e.g. a fixture) must not be silently overridden.
# ---------------------------------------------------------------------------
REPO="${SPIRA_REPO:-}"
REPO_NAME=""
if [ -n "$REPO_ARG" ]; then
    case "$REPO_ARG" in
        */*)  REPO="$REPO_ARG" ;;
        *)    REPO="$(repo_root "$REPO_ARG" 2>/dev/null)" || {
                  printf 'batch: cannot find repo %s in repo-map\n' "$REPO_ARG" >&2
                  exit 2
              }
              REPO_NAME="$REPO_ARG" ;;
    esac
elif [ -z "$REPO" ]; then
    REPO="$(cd "$HERE/.." && pwd -P)"
fi
[ -n "$REPO_NAME" ] || REPO_NAME="$(repo_name_at "$REPO" 2>/dev/null)" || REPO_NAME="$(basename "$REPO")"

# ---------------------------------------------------------------------------
# BASE REF — resolved, never assumed. A wrong base selects nothing (not a fault).
# ---------------------------------------------------------------------------
BASE=""
if [ -n "$REPO_NAME" ]; then
    BASE="$(spira_landref "$REPO_NAME" 2>/dev/null)" || true
fi
if [ -z "$BASE" ]; then
    BASE="$(spira_landref "$REPO" 2>/dev/null)" || {
        printf 'batch: cannot resolve the base ref for %s\n' "${REPO_NAME:-$REPO}" >&2
        printf 'batch: add a base column to the repo-map, or run: git remote set-head origin -a\n' >&2
        exit 2
    }
fi

# ---------------------------------------------------------------------------
# BRANCH WORKTREE — test the branch, not the production checkout.
# Created under the sanctioned root ($SPIRA_RUN/worktree); cleaned on exit.
# Using $$ for uniqueness; a stale entry from a prior crash is pruned first.
# ---------------------------------------------------------------------------
BRANCH_WT="$SPIRA_RUN/worktree/.testbatch-$$"
mkdir -p "$SPIRA_RUN/worktree" 2>/dev/null || {
    printf 'batch: cannot create worktree directory %s\n' "$SPIRA_RUN/worktree" >&2
    exit 2
}
git -C "$REPO" worktree prune 2>/dev/null || true
git -C "$REPO" worktree add -q --detach "$BRANCH_WT" "$BR" 2>/dev/null || {
    printf 'batch: cannot create worktree for %s in %s\n' "$BR" "$REPO" >&2
    exit 2
}
_wt_cleanup() {
    git -C "$REPO" worktree remove -f "$BRANCH_WT" 2>/dev/null || true
    rm -rf "$BRANCH_WT" 2>/dev/null || true
}
trap _wt_cleanup EXIT INT TERM

# PREBUILT BINARIES — a fresh worktree carries only what git tracks, so the
# gate's build job (compiles every workspace binary, downloads them into
# $REPO/bin — sp-7r4rl) would otherwise never reach the container. Copy them
# into the branch worktree so conf.sh's preference for $SPIRA_REPO/bin/<name>
# finds the real compiled binary instead of a suite falling back to building
# its own.
#
# --with-bins supersedes this: $REPO/bin reflects whatever was last built there
# (often stale, or absent entirely outside CI), never necessarily this branch's
# own Rust source. Build the branch's own workspace instead, mirroring the
# gate's build job exactly (make build; copy every executable directly under
# target/release/), so a suite reading bin/ sees this branch's binaries, not a
# leftover from a previous one. A build failure is the candidate's fault.
if [ "$WITH_BINS" = 1 ]; then
    command -v cargo >/dev/null 2>&1 || {
        printf 'batch: --with-bins requires cargo on PATH\n' >&2
        exit 4
    }
    _bins_target_dir="${SPIRA_BATCH_BINS_TARGET_DIR:-$SPIRA_RUN/cargo-target-bins}"
    mkdir -p "$_bins_target_dir" 2>/dev/null || true
    log "batch: --with-bins: building workspace binaries for $BR"
    if ! CARGO_TARGET_DIR="$_bins_target_dir" make -C "$BRANCH_WT" build >&2; then
        printf 'batch: --with-bins: workspace failed to build — candidate fault\n' >&2
        exit 4
    fi
    mkdir -p "$BRANCH_WT/bin"
    find "$_bins_target_dir/release" -maxdepth 1 -type f -executable \
        -exec cp -p {} "$BRANCH_WT/bin/" \; 2>/dev/null || true
elif [ -d "$REPO/bin" ]; then
    mkdir -p "$BRANCH_WT/bin"
    cp -p "$REPO"/bin/* "$BRANCH_WT/bin/" 2>/dev/null || true
fi

# ---------------------------------------------------------------------------
# SUITE DIRECTORY — where to find test-*.sh on the host.
# Defaults to the branch worktree's spira/ dir so suite scripts and the code
# under test both come from the same tree (the branch, not $HERE).
# ---------------------------------------------------------------------------
SUITE_DIR="${SPIRA_BATCH_SUITE_DIR:-$BRANCH_WT/spira}"

# ---------------------------------------------------------------------------
# SUITE SELECTION — a list from one source at a time:
#   --suites <list>  comma-separated names: validate each, use the list directly.
#   --suites -       stdin: read newline-separated names; blank lines ignored.
#                    Empty stdin means "nothing to run" — exit 0, not an error.
#   (default)        diff-derived: delegated to select.sh (the one selector).
#                    Unmapped files do NOT expand to all suites here — the gate
#                    stays cheap; gate-spira.sh (timed run) handles that fallback.
#                    A suite with no # covers: line always runs.
# --suites bypasses diff-derived entirely; only one --suites is accepted.
# ---------------------------------------------------------------------------

SELECTED=""
_SELECTION_TYPE=diff

if [ -n "$SUITES_EXPLICIT" ]; then
    _SELECTION_TYPE=explicit

    if [ "$SUITES_EXPLICIT" = "-" ]; then
        # Read newline-separated suite names from stdin; blank lines ignored.
        # An unknown name is a usage error naming the suite — same as the comma-list path.
        while IFS= read -r _line || [ -n "$_line" ]; do
            _line="${_line#"${_line%%[![:space:]]*}"}"
            _line="${_line%"${_line##*[![:space:]]}"}"
            [ -n "$_line" ] || continue
            if [ ! -r "$SUITE_DIR/$_line" ]; then
                printf 'batch: unknown suite: %s\n' "$_line" >&2
                printf 'batch: suite must exist in %s\n' "$SUITE_DIR" >&2
                exit 2
            fi
            case " $SELECTED " in
                *" $_line "*) ;;
                *) SELECTED="$SELECTED $_line" ;;
            esac
        done
        SELECTED="$(echo $SELECTED)"
        if [ -z "$SELECTED" ]; then
            log "batch: --suites -: empty stdin — nothing to run"
            exit 0
        fi
        _n=0; for _cv_s in $SELECTED; do _n=$((_n + 1)); done
        log "batch: --suites -: selected $_n suite(s) from stdin"

    else
        # Explicit comma-separated list: parse names, validate each against the
        # corpus, and reject an unknown name immediately rather than silently skipping.
        _rest="$SUITES_EXPLICIT"
        while [ -n "$_rest" ]; do
            _s="${_rest%%,*}"
            _rest="${_rest#"$_s"}"
            _rest="${_rest#,}"
            # Strip leading/trailing whitespace.
            _s="${_s#"${_s%%[![:space:]]*}"}"
            _s="${_s%"${_s##*[![:space:]]}"}"
            [ -n "$_s" ] || continue
            if [ ! -r "$SUITE_DIR/$_s" ]; then
                printf 'batch: unknown suite: %s\n' "$_s" >&2
                printf 'batch: suite must exist in %s\n' "$SUITE_DIR" >&2
                exit 2
            fi
            case " $SELECTED " in
                *" $_s "*) ;;  # deduplicate
                *) SELECTED="$SELECTED $_s" ;;
            esac
        done
        SELECTED="$(echo $SELECTED)"  # normalise whitespace
        _n=0; for _cv_s in $SELECTED; do _n=$((_n + 1)); done
        log "batch: --suites: selected $_n explicit suite(s)"
    fi

else
    # Diff-derived selection — delegated to select.sh (the one selector).
    # --no-all-fallback: unmapped files do not expand the selection to all suites.
    # The timed runner (gate-spira.sh) omits this flag and keeps the full fallback;
    # this gate stays cheap (law-absence-needs-a-positive-control covers the timed run).
    _mf="$(mktemp)"
    _sel="$(bash "$HERE/select.sh" \
        --base "$BASE" \
        --head "${SPIRA_GATE_SELECT_HEAD:-$BR}" \
        --repo "$REPO" \
        --suite-dir "$SUITE_DIR" \
        --no-all-fallback \
        --tiers "${SPIRA_BATCH_TIERS:-T2,T3}" \
        --mode-file "$_mf" \
        2>/dev/null || true)"
    _SELECTION_TYPE="$(cat "$_mf" 2>/dev/null || echo diff)"
    rm -f "$_mf"
    SELECTED="$(echo $_sel)"
fi

if [ -z "$SELECTED" ]; then
    log "batch: no suites selected — nothing to do"
    exit 0
fi

# ---------------------------------------------------------------------------
# IMAGE TAG — part of the verdict-cache key: a green against an old image
# must not replay after a dependency is added to the image.
# ---------------------------------------------------------------------------
IMG_TAG="$(bash "$TESTENV" tag 2>/dev/null)" || IMG_TAG="-"

# ---------------------------------------------------------------------------
# BATCH KEY — tree + image tag + selection + this script's own hash.
# Any change to any component produces a new key; a stale verdict is not reused.
# ---------------------------------------------------------------------------
_batch_key() {
    local tree sel_h harness_h
    tree="$(git -C "$REPO" rev-parse --verify -q "$BR^{tree}" 2>/dev/null)" || return 1
    sel_h="$(printf '%s\n' $SELECTED | sort | sha256sum | cut -d' ' -f1)"
    harness_h="$(cat "$0" "$HERE/suite-covers.sh" "$HERE/select.sh" 2>/dev/null | sha256sum | cut -d' ' -f1)"
    [ -n "$harness_h" ] || return 1
    # MODE and _SELECTION_TYPE are included: a serial-green must not replay for a
    # parallel run; a partial-selection green must not replay for an all-corpus run.
    # WITH_BINS is included: a green with $REPO/bin's stale binaries must not replay
    # for a run that builds the branch's own — the two can see different bin/ content
    # for the identical tree.
    printf '%s\n' "$REPO_NAME $tree $IMG_TAG $sel_h $harness_h $MODE $_SELECTION_TYPE $WITH_BINS" | sha256sum | cut -d' ' -f1
}
BATCH_KEY="$(_batch_key 2>/dev/null || true)"

# ---------------------------------------------------------------------------
# RESULTS DIRECTORY
# ---------------------------------------------------------------------------
RESULTS_ROOT="${SPIRA_BATCH_RESULTS:-$SPIRA_RUN/batch-results}"
if [ -n "$BATCH_KEY" ]; then
    RESULTS="$RESULTS_ROOT/$BATCH_KEY"
else
    RESULTS="$RESULTS_ROOT/$(date +%s)-$$"
fi
mkdir -p "$RESULTS"

# Timing ledger — runner shape captured once at batch start; suites wall time
# and CPU measured around the actual suite loop further below.
_timing_nproc="$(nproc 2>/dev/null || printf '-')"
_timing_memtotal_kb="$(awk '/^MemTotal:/{print $2; exit}' /proc/meminfo 2>/dev/null || printf '-')"
_suites_t0=0
_suites_wall=0
_timing_cpu0="0 0"
_timing_cpu1="0 0"

# ---------------------------------------------------------------------------
# SUITE STATE — read from the candidate tree, never from the installed harness.
# Disabled suites are pre-empted here (before the container starts); their
# result files carry status=disabled. Quarantined suites run normally but a
# non-zero exit is recorded as quarantined-red rather than red and does not
# increment _batch_red. Fail-closed: an absent or unreadable state file means
# every suite is active (blocking).
# ---------------------------------------------------------------------------
_STS_TMP="$(mktemp)"
_STS_QUARANTINED=""
git -C "$REPO" show "${BR}:${SPIRA_SUITE_STATE_FILE:-spira/suite-state}" > "$_STS_TMP" 2>/dev/null || true
_SELECTED_ACTIVE=""
for _sts_s in $SELECTED; do
    case "$(suite_state_of "$_STS_TMP" "$_sts_s")" in
        disabled)
            printf '%s %s %s %s %s%s -\n' disabled "$(date +%s)" 0 - "$MODE" \
                " $_SELECTION_TYPE" > "$RESULTS/$_sts_s.result"
            : > "$RESULTS/$_sts_s.out"
            printf '  %-32s DISABLED\n' "$_sts_s"
            ;;
        quarantined)
            _STS_QUARANTINED="$_STS_QUARANTINED $_sts_s"
            _SELECTED_ACTIVE="$_SELECTED_ACTIVE $_sts_s"
            ;;
        *)
            _SELECTED_ACTIVE="$_SELECTED_ACTIVE $_sts_s"
            ;;
    esac
done
rm -f "$_STS_TMP"; unset _STS_TMP
SELECTED="$(echo $_SELECTED_ACTIVE)"
unset _SELECTED_ACTIVE _sts_s
if [ -z "$SELECTED" ]; then
    log "batch: all suites pre-empted by lifecycle state — nothing to run"
    exit 0
fi

# ---------------------------------------------------------------------------
# VERDICT CACHE — check before starting the container.
# ---------------------------------------------------------------------------
VERDICT_DIR="${SPIRA_VERDICTS:-$SPIRA_RUN/verdicts}"
verdict_ttl="${SPIRA_VERDICT_TTL:-86400}"
_verdict_override_reason=""
case "$verdict_ttl" in ''|*[!0-9]*) verdict_ttl=0 ;; esac

if [ -n "$BATCH_KEY" ] && [ "$verdict_ttl" -gt 0 ] && \
   [ -r "$VERDICT_DIR/batch-$BATCH_KEY" ]; then
    _cached_at="" _cached_when="" _cached_by="" _cached_verdict="" _cached_red_suites="" _cached_override_reason=""
    # shellcheck disable=SC1090
    eval "$(sed -n 's/^\(when\|by\|at\|verdict\|red_suites\|override_reason\)=\(.*\)$/_cached_\1="\2"/p' \
        "$VERDICT_DIR/batch-$BATCH_KEY" 2>/dev/null)"
    _age=-1
    case "${_cached_at:-}" in ''|*[!0-9]*) : ;; *) _age=$(( $(date +%s) - _cached_at )) ;; esac
    if [ "$_age" -ge 0 ] && [ "$_age" -lt "$verdict_ttl" ]; then
        # Absent verdict field means an old green-only file — treat as green.
        case "${_cached_verdict:-green}" in
            green)
                log "batch: tree already passed at ${_cached_when:-unknown} — key batch-$BATCH_KEY"
                exit 0
                ;;
            red)
                _repeat_reason="${SPIRA_VERDICT_REPEAT_CONSIDERED:-}"
                if [ -n "$_repeat_reason" ] && [ "${#_repeat_reason}" -ge 10 ]; then
                    log "batch: repeat allowed — reason: $_repeat_reason"
                    # Store for inclusion in the new verdict written after the run.
                    _verdict_override_reason="$_repeat_reason"
                else
                    [ -n "$_repeat_reason" ] && \
                        log "batch: SPIRA_VERDICT_REPEAT_CONSIDERED must be a sentence (min 10 chars)"
                    log "batch: repeat attempt refused — prior red at ${_cached_when:-unknown} — key batch-$BATCH_KEY — red suites: ${_cached_red_suites:-(unknown)} — to override: SPIRA_VERDICT_REPEAT_CONSIDERED='<reason, min 10 chars>' bash spira/testenv-batch.sh ..."
                    _bead_cmd="${SPIRA_BATCH_INCIDENT_CMD:-$HERE/incident.sh}"
                    if [ -r "$_bead_cmd" ] && [ -n "${SPIRA_DB:-}" ]; then
                        _suites_csv="${_cached_red_suites// /,}"
                        { printf '%s\n\nTwo routes forward:\n1. Commit a fix — the new tree produces a new key and the cache does not apply.\n2. If the red was environmental (not a code defect), re-run with SPIRA_VERDICT_REPEAT_CONSIDERED set to a sentence describing why (min 10 chars): SPIRA_VERDICT_REPEAT_CONSIDERED="<reason>" bash spira/testenv-batch.sh --suites %s %s\n' \
                            "Repeat attempt refused. Prior red at ${_cached_when:-unknown}. Key: batch-$BATCH_KEY. Red suites: ${_cached_red_suites:-(unknown)}. Branch: $BR. SPIRA_VERDICT_REPEAT_CONSIDERED was not set or was too short." \
                            "${_suites_csv:-(unknown)}" "$BR"
                          [ -n "${_cached_override_reason:-}" ] && \
                            printf 'Prior override attempted: %s\n' "$_cached_override_reason"
                        } | SPIRA_INCIDENT_REF="repeat-refused:$BR:${BATCH_KEY:0:16}" \
                            SPIRA_INCIDENT_REPO="$(spira_home_repo 2>/dev/null)" \
                            SPIRA_INCIDENT_TYPE=task \
                            SPIRA_INCIDENT_CAUSE=repeat-refused \
                            SPIRA_INCIDENT_PRIORITY=3 \
                            SPIRA_INCIDENT_DELIVERS=action \
                            bash "$_bead_cmd" file \
                                "repeat attempt: no change — $BR" - 2>/dev/null || true
                    fi
                    exit 2
                fi
                ;;
        esac
    fi
fi

# ---------------------------------------------------------------------------
# FINGERPRINT — same normalisation as suites.sh so dedup keys match across
# callers. Duplicated here rather than placed in lib.sh so that each caller
# can evolve independently, and the dependency is explicit.
# ---------------------------------------------------------------------------
_fp() {  # _fp <rc> <output> -> short stable digest
    local rc="$1" out="$2" sig
    # || true: grep exits 1 on no match; pipefail would fail the assignment.
    sig="$(printf '%s\n' "$out" | grep -F 'FAIL' || true)"
    [ -n "$sig" ] || sig="$(printf '%s\n' "$out" | tail -n 20)"
    printf 'rc=%s\n%s\n' "$rc" "$sig" \
        | sed -e 's#/tmp/[A-Za-z0-9._-]*#/tmp/X#g' \
              -e 's#/[A-Za-z0-9._/-]*/sptest_[A-Za-z0-9_]*#/X#g' \
              -e 's/[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9][.0-9]*Z\{0,1\}/TIMESTAMP/g' \
              -e 's/[0-9][0-9]:[0-9][0-9]:[0-9][0-9]/TIME/g' \
              -e 's/[0-9]\{3,\}/N/g' \
        | cksum | tr -d ' \t'
}

# ---------------------------------------------------------------------------
# CONTAINER NAME — derived from the batch instance so the test can set
# SPIRA_BATCH_INSTANCE and predict the container name for lifecycle operations.
# ---------------------------------------------------------------------------
_inst_default="${BATCH_KEY:0:12}"
[ -n "$_inst_default" ] || _inst_default="$(date +%s)-$$"
INSTANCE="${SPIRA_BATCH_INSTANCE:-$_inst_default}"
CNAME="spira-batch-${INSTANCE}"
# Register with a landing pass so halt can tear us down by name.
[ -n "${SPIRA_LANDING_CONTAINERS:-}" ] && \
    printf '%s\n' "$CNAME" >> "$SPIRA_LANDING_CONTAINERS" 2>/dev/null || true

_batch_tmp="$(mktemp)"
_par_tmp=""  # set in parallel block; empty means serial mode was used

# Owner file: a host-side record that maps this container to our PID, so a
# later batch (or this one at startup) can identify and sweep containers whose
# owner process has died without cleaning up.
_BATCH_OWNER_FILE="/tmp/${CNAME}.owner"

_BATCH_HOME="/tmp/spira-batch-${INSTANCE}"

_batch_cleanup() {
    bash "$TESTENV" down --name "$CNAME" >/dev/null 2>&1 || \
        log "batch: teardown failed for $CNAME — checking whether it survived"
    rm -rf "$_BATCH_HOME" 2>/dev/null || true
    # _batch_owner_release unlinks the owner file only once the container is
    # confirmed gone from podman — a failed teardown that leaves it running
    # must leave the owner file too, or the orphan sweep below can never find it.
    _batch_owner_release "$CNAME" "$_BATCH_OWNER_FILE" || \
        log "batch: container $CNAME survived teardown — leaving owner file for the orphan sweep"
    _wt_cleanup
    rm -f "$_batch_tmp"
    [ -n "$_par_tmp" ] && rm -rf "$_par_tmp" || true
}
trap _batch_cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# ORPHAN SWEEP — two arms, from batch-owner.sh. Runs before we start our own
# container so a crashed previous run does not consume memory for the duration
# of this one.
#
#   1. owner file present but the owner PID has exited (as before).
#   2. a spira-batch-* container with NO owner file at all, older than
#      SPIRA_BATCH_ORPHAN_MIN_AGE. A container can lose its owner file without
#      ever dying — a failed `testenv down` used to delete it unconditionally,
#      and /tmp is separately subject to systemd-tmpfiles ageing — and arm 1
#      can never see a container with no owner file to read a PID from.
# ---------------------------------------------------------------------------
while IFS= read -r _sw_line; do
    [ -n "$_sw_line" ] && log "batch: $_sw_line"
done < <(_batch_sweep_dead_owners)

while IFS= read -r _sw_line; do
    [ -n "$_sw_line" ] && log "batch: $_sw_line"
done < <(_batch_sweep_ownerless "${SPIRA_BATCH_ORPHAN_MIN_AGE:-3600}")

printf '%s\n' "$$" > "$_BATCH_OWNER_FILE"
mkdir -p "$_BATCH_HOME" 2>/dev/null || true

# ---------------------------------------------------------------------------
# CONTAINER UP
# ---------------------------------------------------------------------------
log "batch: starting container $CNAME (branch $BR)"
bash "$TESTENV" up --name "$CNAME" --checkout "$BRANCH_WT" >&2 || {
    log "batch: container $CNAME did not come up"
    exit 2
}

if ! bash "$TESTENV" probe --name "$CNAME"; then
    log "batch: probe failed — user systemd not available in $CNAME"
    exit 2
fi

# ---------------------------------------------------------------------------
# INSTALL — configure then install, so each step's failures are distinct.
# SPIRA_INSTALL_FORCE=1: the checkout is on a topic branch, not the landref;
# the force flag is the documented override for this exact case.
# ---------------------------------------------------------------------------
if [ -z "${SPIRA_BATCH_SKIP_INSTALL:-}" ]; then
    log "batch: configure inside $CNAME"
    podman exec --user "$_SPIRA_USER" \
        -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
        -e "DBUS_SESSION_BUS_ADDRESS=unix:path=${_USER_RUNTIME}/bus" \
        -e "CARGO_HOME=${_CONTAINER_CARGO}" \
        -e "CARGO_TARGET_DIR=${_CONTAINER_CARGO_TARGET}" \
        -e "CONFIGURE_PROD=${_CONTAINER_WORKSPACE}/spira" \
        -e "CONFIGURE_MAX_AEONS=1" \
        -e "CONFIGURE_MAX_LIVE_AEONS=1" \
        -e "CONFIGURE_LOOM_ADDR=127.0.0.1:7300" \
        -e "CONFIGURE_DOLT_DATA=" \
        "$CNAME" bash "${_CONTAINER_WORKSPACE}/spira/configure.sh" >&2 || {
        log "batch: configure failed — harness fault"
        exit 3
    }

    # Loom and the cockpit panel are Rust; the container image ships an older rustc
    # that cannot build lockfile v4, so their binaries never exist in the container.
    # Suspend them in the control plane so systemd/install.sh skips their units rather
    # than enabling services whose ExecStart target is absent.
    log "batch: suspending Rust-backed units inside $CNAME (image rustc too old for lockfile v4)"
    for _u in spira-loom spira-cockpit; do
        podman exec --user "$_SPIRA_USER" \
            -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
            -e "SPIRA_RUN=/tmp/spira-batch-${INSTANCE}" \
            "$CNAME" bash "${_CONTAINER_WORKSPACE}/spira/ctrl.sh" suspend "$_u" \
                --reason "image rustc too old for lockfile v4" --owner sp-fud1 >&2 || {
            log "batch: ctrl suspend failed for $_u — harness fault"
            exit 3
        }
    done

    log "batch: install instance $INSTANCE inside $CNAME"
    podman exec --user "$_SPIRA_USER" \
        -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
        -e "DBUS_SESSION_BUS_ADDRESS=unix:path=${_USER_RUNTIME}/bus" \
        -e "CARGO_HOME=${_CONTAINER_CARGO}" \
        -e "CARGO_TARGET_DIR=${_CONTAINER_CARGO_TARGET}" \
        -e "SPIRA_INSTALL_FORCE=1" \
        -e "SPIRA_RUN=/tmp/spira-batch-${INSTANCE}" \
        -e "SPIRA_TESTDB_DATA=/tmp/spira-batch-${INSTANCE}/testdb" \
        "$CNAME" bash "${_CONTAINER_WORKSPACE}/systemd/install.sh" "$INSTANCE" >&2 || {
        log "batch: install failed — harness fault"
        exit 3
    }
fi

# ---------------------------------------------------------------------------
# BD TIMING SHIM — inject a bd wrapper so per-suite bd call time can be
# separated from suite script time.  Fail-soft: if injection fails the batch
# continues; timing.tsv will carry "-" for bd_ms on every suite.
# ---------------------------------------------------------------------------
_bd_timing_dir="/tmp/bd-timing-${INSTANCE}"
_bd_shim_dir="/tmp/bd-shim-${INSTANCE}"
_bd_shim_path="${_bd_shim_dir}:/usr/local/bin:/usr/bin:/bin"
{
    podman exec --user "$_SPIRA_USER" "$CNAME" \
        mkdir -p "$_bd_shim_dir" "$_bd_timing_dir" >/dev/null 2>&1
    podman exec --user "$_SPIRA_USER" "$CNAME" bash -c "
        printf '#!/usr/bin/env bash\nexec ${_CONTAINER_WORKSPACE}/spira/bd-shim.sh \"\$@\"\n' \
            > '${_bd_shim_dir}/bd' && chmod +x '${_bd_shim_dir}/bd'
    " >/dev/null 2>&1
    _cpath="$(podman exec --user "$_SPIRA_USER" "$CNAME" printenv PATH 2>/dev/null)"
    [ -n "${_cpath:-}" ] && _bd_shim_path="${_bd_shim_dir}:${_cpath}"
} 2>/dev/null || true

# ---------------------------------------------------------------------------
# REQUIREMENTS CHECK — suites declaring # requires: tokens are pre-checked
# inside the container. An unmet requirement records skip-req (distinct from
# exit-77 self-skip) with the missing token named in the fingerprint field.
# The suite is removed from the run list, so it is never "unreached".
#
# Each unique token is checked exactly once via `command -v` inside the
# container; the result is cached so a token shared by several suites costs
# one podman exec, not one per suite.
#
# "testenv" is not a binary: it declares that the suite touches a user
# manager, an install/uninstall path, or production paths (sp-nxvjm) and must
# refuse outside this container. Reaching this loop at all means we already
# are inside it, so the token is always met here — `command -v testenv` would
# find nothing and wrongly skip-req every suite that declares it.
# ---------------------------------------------------------------------------
_req_all_tokens=""
for _rs in $SELECTED; do
    _reqs="$(suite_requires_of "$SUITE_DIR/$_rs")"
    for _tok in $_reqs; do
        [ -n "$_tok" ] || continue
        [ "$_tok" = testenv ] && continue
        case " $_req_all_tokens " in
            *" $_tok "*) ;;
            *) _req_all_tokens="$_req_all_tokens $_tok" ;;
        esac
    done
done

_req_unmet=""
if [ -n "$_req_all_tokens" ]; then
    for _tok in $_req_all_tokens; do
        if podman exec --user "$_SPIRA_USER" \
               -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
               "$CNAME" bash -c "command -v '$_tok' >/dev/null 2>&1"; then
            : # met — not listed in _req_unmet
        else
            _req_unmet="$_req_unmet $_tok"
            log "batch: requirement not met in container: $_tok"
        fi
    done
fi

_SELECTED_RUNNABLE=""
for _rs in $SELECTED; do
    _reqs="$(suite_requires_of "$SUITE_DIR/$_rs")"
    if [ -z "$_reqs" ]; then
        _SELECTED_RUNNABLE="$_SELECTED_RUNNABLE $_rs"
        continue
    fi
    _missing=""
    for _tok in $_reqs; do
        [ -n "$_tok" ] || continue
        case " $_req_unmet " in
            *" $_tok "*) _missing="${_missing:+$_missing,}$_tok" ;;
        esac
    done
    if [ -n "$_missing" ]; then
        printf '%s %s %s %s %s%s -\n' skip-req "$(date +%s)" 0 "requires:$_missing" "$MODE" \
            " $_SELECTION_TYPE" > "$RESULTS/$_rs.result"
        : > "$RESULTS/$_rs.out"
        printf '  %-32s SKIP-REQ requires:%s\n' "$_rs" "$_missing"
    else
        _SELECTED_RUNNABLE="$_SELECTED_RUNNABLE $_rs"
    fi
done
SELECTED="$(echo $_SELECTED_RUNNABLE)"
if [ -z "$SELECTED" ]; then
    log "batch: all suites pre-empted by unmet requirements — nothing to run"
    exit 0
fi

# ---------------------------------------------------------------------------
# SHARED TESTDB BASELINE — build the fixture database once; suites derive
# private copies via testdb_up's fast-path (cp .beads, ~26ms) instead of
# each running bd init (~6s). Failure falls back to per-suite databases.
# ---------------------------------------------------------------------------
_TESTDB_SHARED_NAME=""
_TESTDB_SHARED_DIR=""
_TESTDB_SHARED_BASELINE=""
_TESTDB_SHARED_BD=""
_TESTDB_SHARED_BIN=""
_TESTDB_SHARED_MODE=""
_TESTDB_BATCH_SHARED=0

_baseline_tmp="$(mktemp)"
log "batch: building shared testdb baseline in $CNAME"
if podman exec --user "$_SPIRA_USER" \
       -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
       -e "SPIRA_TESTDB_DATA=/tmp/spira-batch-${INSTANCE}/testdb" \
       "$CNAME" bash -c \
       '. /workspace/spira/testdb.sh && testdb_up batch_baseline && printf "TESTDB_NAME=%s\nTESTDB_DIR=%s\nTESTDB_BASELINE=%s\nTESTDB_BD=%s\nTESTDB_BIN=%s\nTESTDB_MODE=%s\n" "$TESTDB_NAME" "$TESTDB_DIR" "${TESTDB_BASELINE:-}" "$TESTDB_BD" "${TESTDB_BIN:-}" "${TESTDB_MODE:-}"' \
       >"$_baseline_tmp" 2>&1; then
    while IFS= read -r _bl; do
        case "$_bl" in
            TESTDB_NAME=*)     _TESTDB_SHARED_NAME="${_bl#TESTDB_NAME=}" ;;
            TESTDB_DIR=*)      _TESTDB_SHARED_DIR="${_bl#TESTDB_DIR=}" ;;
            TESTDB_BASELINE=*) _TESTDB_SHARED_BASELINE="${_bl#TESTDB_BASELINE=}" ;;
            TESTDB_BD=*)       _TESTDB_SHARED_BD="${_bl#TESTDB_BD=}" ;;
            TESTDB_BIN=*)      _TESTDB_SHARED_BIN="${_bl#TESTDB_BIN=}" ;;
            TESTDB_MODE=*)     _TESTDB_SHARED_MODE="${_bl#TESTDB_MODE=}" ;;
        esac
    done <"$_baseline_tmp"
    if [ -n "$_TESTDB_SHARED_NAME" ] && [ -n "$_TESTDB_SHARED_BASELINE" ]; then
        _TESTDB_BATCH_SHARED=1
        log "batch: shared testdb baseline ready (mode: ${_TESTDB_SHARED_MODE:-?}, name: ${_TESTDB_SHARED_NAME})"
    else
        log "batch: shared testdb baseline built but vars incomplete — falling back to per-suite databases"
    fi
else
    log "batch: shared testdb baseline build failed — falling back to per-suite databases"
    cat "$_baseline_tmp" >&2
fi
rm -f "$_baseline_tmp"

# _testdb_env: per-suite testdb environment flags for podman exec.
# Shared: suites copy the baseline (~26ms each); Private: each suite runs bd init (~6s).
if [ "$_TESTDB_BATCH_SHARED" = 1 ]; then
    _testdb_env=(
        -e "TESTDB_SHARED=1"
        -e "TESTDB_NAME=${_TESTDB_SHARED_NAME}"
        -e "TESTDB_DIR=${_TESTDB_SHARED_DIR}"
        -e "TESTDB_BASELINE=${_TESTDB_SHARED_BASELINE}"
        -e "TESTDB_BD=${_TESTDB_SHARED_BD}"
        -e "TESTDB_BIN=${_TESTDB_SHARED_BIN}"
        -e "TESTDB_MODE=${_TESTDB_SHARED_MODE}"
    )
else
    _testdb_env=(
        -e "TESTDB_SHARED=0"
        -e "TESTDB_NAME="
        -e "TESTDB_DIR="
    )
fi

# ---------------------------------------------------------------------------
# RUN SUITES — serial or parallel, per --mode.
#
# MODE is recorded as the 5th field of every result file: a green under serial
# is a weaker claim than a green under parallel; the record must not conflate them.
#
# PARALLEL isolation (each suite gets its own):
#   HOME:           each suite runs with a private home directory. $HOME/.config and
#                   $HOME/.local are per-suite, so unit fixture files written by one
#                   suite to $HOME/.config/systemd/user are invisible to neighbours.
#                   The batch install step (run before suites) uses the container
#                   user's real home; per-suite HOME means suites cannot see those
#                   batch-installed units either, preventing false "unexpected file"
#                   failures in suites that scan $HOME/.config/systemd/user.
#                   LIMIT: the shared systemd user daemon is bound to the container
#                   UID, not to HOME. Suites that call real systemctl --user connect
#                   to the same daemon regardless of HOME and must isolate via
#                   SPIRA_INSTANCE unit-name namespacing, not HOME. See SCAR below.
#   SPIRA_INSTANCE: units are named per-instance; parallel suites cannot collide on
#                   installed unit names.
#   SPIRA_RUN:      each suite's temp state is isolated at a distinct path.
#   TESTDB_NAME:    when the shared baseline is available (TESTDB_BATCH_SHARED=1),
#                   each suite copies .beads from TESTDB_BASELINE (~26ms); without
#                   it, TESTDB_SHARED=0 + empty TESTDB_NAME → testdb.sh generates
#                   a unique name and runs bd init (~6s) per suite.
#
# SCAR: test-install-migrate.sh and test-install-instance.sh plant legacy systemd
# unit fixtures (spira-sentinel.service etc.) in the real systemd unit directory via
# `systemctl --user enable`. With per-suite HOME those suites need to either use
# SPIRA_INSTANCE namespacing or declare a skip when HOME is not the real container
# home (XDG_RUNTIME_DIR check already acts as a container guard; HOME can extend it).
# ---------------------------------------------------------------------------
#!maxpar-begin
# Derive maxpar from the guest's own hardware: min(nproc, floor((avail-reserve)/per_suite)).
# SPIRA_BATCH_MAXPAR is a ceiling: when set, the result is min(N, hardware_maxpar).
# An operator value set for a larger box cannot exceed what this box allows.
# Setting it to 0 disables all capping (useful for small explicit selections or stress tests).
_mem_reserve_mib="${SPIRA_BATCH_MEM_RESERVE_MIB:-1024}"
_mem_per_suite_mib="${SPIRA_BATCH_MEM_PER_SUITE_MIB:-192}"
_maxpar_cpu="$(nproc)"
_mem_avail_mib="${SPIRA_BATCH_MEM_AVAIL_MIB:-$(awk '/^MemAvailable:/{printf "%d", $2/1024}' /proc/meminfo 2>/dev/null || echo 0)}"
_mem_budget=$(( _mem_avail_mib - _mem_reserve_mib ))
[ "${_mem_budget:-0}" -lt "${_mem_per_suite_mib}" ] && _mem_budget="${_mem_per_suite_mib}"
_mem_bound=$(( _mem_budget / _mem_per_suite_mib ))
[ "${_mem_bound:-0}" -lt 1 ] && _mem_bound=1
if [ "${_maxpar_cpu}" -le "${_mem_bound}" ]; then
    _hardware_maxpar="${_maxpar_cpu}"
    _hardware_binding="cpu"
else
    _hardware_maxpar="${_mem_bound}"
    _hardware_binding="memory"
fi
if [ -n "${SPIRA_BATCH_MAXPAR:-}" ] && [ "${SPIRA_BATCH_MAXPAR}" = "0" ]; then
    _maxpar=0
    _maxpar_binding="override-unlimited"
elif [ -n "${SPIRA_BATCH_MAXPAR:-}" ] && [ "${SPIRA_BATCH_MAXPAR}" -le "${_hardware_maxpar}" ] 2>/dev/null; then
    _maxpar="${SPIRA_BATCH_MAXPAR}"
    _maxpar_binding="override"
elif [ -n "${SPIRA_BATCH_MAXPAR:-}" ]; then
    _maxpar="${_hardware_maxpar}"
    _maxpar_binding="${_hardware_binding}"
else
    _maxpar="${_hardware_maxpar}"
    _maxpar_binding="${_hardware_binding}"
fi
#!maxpar-end

_n_selected=0; for _s in $SELECTED; do _n_selected=$((_n_selected+1)); done

# ---------------------------------------------------------------------------
# RUN ID — identifies this batch in the suite-times ledger.
# GITHUB_RUN_ID is set by GitHub Actions; local runs use a timestamp.
# ---------------------------------------------------------------------------
_BATCH_RUN_ID="${GITHUB_RUN_ID:-${SPIRA_BATCH_RUN_ID:-local-$(date +%s)}}"
_shim_spira_path=""

# _batch_bd_read <log-path-inside-container> — print "<calls>\t<ms>"
_batch_bd_read() {
    podman exec --user "$_SPIRA_USER" \
        -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
        "$CNAME" awk 'BEGIN{n=0;s=0}{n++;s+=$1}END{printf "%d\t%d",n,s}' \
        "$1" 2>/dev/null || printf '0\t0'
}

# _append_suite_times <suite> <rc> <wall_secs> <bd_calls> <bd_ms> <mode>
# The single producer of a suite's timing (sp-au8a7): appends into run/tsd/'s suite-timing
# family. Best-effort — an unbuilt tsd-write leaves the family file simply absent, which
# never fails the suite it is watching.
_append_suite_times() {
    _tsd_suite_timing "$1" "$2" "$3" "$4" "$5" "$6"
}

# _tsd_suite_timing <suite> <rc> <wall_secs> <bd_calls> <bd_ms> <mode>
_tsd_suite_timing() {
    local bin="${SPIRA_TSD_BIN:-}"
    [ -n "$bin" ] && [ -x "$bin" ] || return 0
    "$bin" --family suite-timing --root "${SPIRA_RUN:-}" \
        --field-str "run_id=${_BATCH_RUN_ID:-}" --field-str "branch=${BR:-}" \
        --field-str "suite=$1" --field "rc=$2" --field "wall_secs=$3" \
        --field "bd_calls=$4" --field "bd_ms=$5" --field-str "mode=$6" \
        >/dev/null 2>&1 || true
}

# _lpt_order <space-separated suite list> — print the same suites reordered
# longest-first (LPT), using historical wall_secs from the run/tsd/
# suite-timing family (sp-sbc6o) via tsd-query.sh's by-group query. LPT alone
# brings a parallel pool's makespan close to
# max(longest suite, total-suite-seconds/maxpar) (sp-ezkp3) — but only once
# the slowest suites start first instead of draining an emptying pool at the
# tail (sp-rbulh).
#
# A suite absent from the ledger — new, or the ledger unreadable at all —
# sorts as if it were the longest suite on record: an unmeasured suite is
# exactly the one a late start costs the most, so absence never falls back to
# list order for everyone, it only ever grows the set treated as longest.
_lpt_order() {
    local _pool="$1"
    [ -z "$_pool" ] && return 0
    local _tab; _tab="$(printf '\t')"
    local _durations=""
    if [ -x "$HERE/tsd-query.sh" ]; then
        _durations="$(bash "$HERE/tsd-query.sh" by-group suite-timing suite wall_secs 2>/dev/null)"
    fi
    local _longest=0 _lg_s _lg_d
    while IFS="$_tab" read -r _lg_s _lg_d; do
        case "${_lg_d:-}" in ''|*[!0-9.]*) continue ;; esac
        awk -v a="$_lg_d" -v b="$_longest" 'BEGIN{exit !(a>b)}' && _longest="$_lg_d"
    done < <(printf '%s\n' "$_durations")
    local _sentinel; _sentinel="$(awk -v m="$_longest" 'BEGIN{printf "%.6f", m+1}')"

    local _idx=0 _s _d _rows=""
    for _s in $_pool; do
        _idx=$((_idx+1))
        _d="$(printf '%s\n' "$_durations" | awk -F'\t' -v s="$_s" '$1==s{print $2; exit}')"
        case "${_d:-}" in ''|*[!0-9.]*) _d="$_sentinel" ;; esac
        _rows="${_rows}${_d}${_tab}${_idx}${_tab}${_s}
"
    done
    printf '%s' "$_rows" | sort -t "$_tab" -k1,1nr -k2,2n | awk -F'\t' '{printf "%s ", $3}'
}

_suites_t0="$(date +%s)"
_timing_cpu0="$(awk '/^cpu /{s=0;for(i=2;i<=NF;i++)s+=$i;idle=$6+$7;printf "%d %d",s,idle;exit}' \
    /proc/stat 2>/dev/null || printf '0 0')"

if [ "$MODE" = parallel ]; then
    if [ "${_maxpar:-0}" -gt 0 ] 2>/dev/null; then
        case "${_maxpar_binding}" in
            override)
                log "batch: running $_n_selected suite(s) in $CNAME (mode: $MODE, maxpar: $_maxpar [override: SPIRA_BATCH_MAXPAR=${SPIRA_BATCH_MAXPAR:-?}; hardware was ${_hardware_binding}-bound at ${_hardware_maxpar}])" ;;
            *)
                log "batch: running $_n_selected suite(s) in $CNAME (mode: $MODE, maxpar: $_maxpar [${_maxpar_binding}-bound: cpu=${_maxpar_cpu} mem=${_mem_avail_mib}MiB avail ${_mem_reserve_mib}MiB reserve ${_mem_per_suite_mib}MiB/suite])" ;;
        esac
    else
        log "batch: running $_n_selected suite(s) in $CNAME (mode: $MODE, maxpar: unlimited)"
    fi
else
    log "batch: running $_n_selected suite(s) in $CNAME (mode: $MODE, nproc: $(nproc))"
fi

_BATCH_T0="$(date +%s)"
_batch_red=0
_batch_quarantined_red=0
_batch_container_dead=0
_batch_container_fault_detail=""
_batch_exec_fault=0
_batch_exec_fault_n=0  # consecutive rc!=0 after 0s with no output (serial mode)

# Per-suite timeout: 0 disables; default 600 seconds, mirroring gate-spira.sh.
# The `timeout` command exits 124 when the limit fires; we map that to status=timeout
# in the result file so callers can distinguish a runaway from a genuine red.
_suite_timeout="${SPIRA_SUITE_TIMEOUT:-600}"

# _container_check_live — sets _batch_container_dead=1 when the container is
# confirmed dead. A single failed or empty inspect is transient; only a
# definitive Running=false, or N consecutive non-true results, declares death.
# Records ExitCode and OOMKilled in _batch_container_fault_detail.
_container_check_live() {
    local _cl_n=0 _cl_result
    local _cl_max="${SPIRA_BATCH_LIVENESS_RETRIES:-3}"
    local _cl_sleep="${SPIRA_BATCH_LIVENESS_SLEEP:-3}"
    while [ "$_cl_n" -lt "$_cl_max" ]; do
        _cl_result="$(podman container inspect \
            --format '{{.State.Running}}' "$CNAME" 2>/dev/null || true)"
        [ "$_cl_result" = "true"  ] && return 0   # alive
        [ "$_cl_result" = "false" ] && break       # definitively exited
        _cl_n=$((_cl_n + 1))
        [ "$_cl_n" -lt "$_cl_max" ] || break
        log "batch: container inspect returned empty (attempt $_cl_n/$_cl_max) — retrying in ${_cl_sleep}s"
        sleep "$_cl_sleep"
    done
    local _cl_exit _cl_oom
    _cl_exit="$(podman container inspect \
        --format '{{.State.ExitCode}}' "$CNAME" 2>/dev/null || true)"
    _cl_oom="$(podman container inspect \
        --format '{{.State.OOMKilled}}' "$CNAME" 2>/dev/null || true)"
    _batch_container_dead=1
    _batch_container_fault_detail="ExitCode=${_cl_exit:-?} OOMKilled=${_cl_oom:-?}"
}

if [ "$MODE" = serial ]; then

    # Serial: one suite at a time. Suites share SPIRA_RUN inside the container.
    # Container-death check runs after each suite so the remaining ones are
    # marked unreached rather than never recorded.
    for s in $SELECTED; do
        t0="$(date +%s)"
        out_file="$RESULTS/$s.out"
        res_file="$RESULTS/$s.result"

        _rc=0
        _serial_bd_log="/tmp/spira-batch-${INSTANCE}/bd/${s}.log"
        # Wrap with `timeout` when the limit is non-zero. On timeout, `timeout`
        # kills the podman exec client (rc=124) and we record status=timeout rather
        # than red so the two failure kinds stay distinguishable.
        if [ "${_suite_timeout:-0}" -gt 0 ] 2>/dev/null; then
            timeout "$_suite_timeout" podman exec --user "$_SPIRA_USER" \
                -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
                -e "DBUS_SESSION_BUS_ADDRESS=unix:path=${_USER_RUNTIME}/bus" \
                -e "CARGO_HOME=${_CONTAINER_CARGO}" \
                -e "CARGO_TARGET_DIR=${_CONTAINER_CARGO_TARGET}" \
                -e "SPIRA_IN_TESTENV=1" \
                "${_testdb_env[@]}" \
                -e "TMUX=" \
                -e "SPIRA_PATH=${_shim_spira_path}" \
                -e "SPIRA_BD_LOG=${_serial_bd_log}" \
                -e "SPIRA_RUN=/tmp/spira-batch-${INSTANCE}" \
                -e "SPIRA_TESTDB_DATA=/tmp/spira-batch-${INSTANCE}/testdb" \
                -e "SPIRA_BD_TIMING_LOG=${_bd_timing_dir}/${s}.log" \
                "$CNAME" bash "${_CONTAINER_WORKSPACE}/spira/$s" >"$_batch_tmp" 2>&1 || _rc=$?
        else
            podman exec --user "$_SPIRA_USER" \
                -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
                -e "DBUS_SESSION_BUS_ADDRESS=unix:path=${_USER_RUNTIME}/bus" \
                -e "CARGO_HOME=${_CONTAINER_CARGO}" \
                -e "CARGO_TARGET_DIR=${_CONTAINER_CARGO_TARGET}" \
                -e "SPIRA_IN_TESTENV=1" \
                "${_testdb_env[@]}" \
                -e "TMUX=" \
                -e "SPIRA_PATH=${_shim_spira_path}" \
                -e "SPIRA_BD_LOG=${_serial_bd_log}" \
                -e "SPIRA_RUN=/tmp/spira-batch-${INSTANCE}" \
                -e "SPIRA_TESTDB_DATA=/tmp/spira-batch-${INSTANCE}/testdb" \
                -e "SPIRA_BD_TIMING_LOG=${_bd_timing_dir}/${s}.log" \
                "$CNAME" bash "${_CONTAINER_WORKSPACE}/spira/$s" >"$_batch_tmp" 2>&1 || _rc=$?
        fi

        out="$(cat "$_batch_tmp")" || true
        secs=$(( $(date +%s) - t0 ))

        # Exec-storm detection: consecutive non-zero exits after 0s with no output
        # means podman exec is failing immediately rather than suites running and failing.
        if [ "$_rc" -ne 0 ] && [ "$secs" -eq 0 ] \
               && [ -z "$(printf '%s' "$out" | tr -d '[:space:]')" ]; then
            _batch_exec_fault_n=$((_batch_exec_fault_n + 1))
        else
            _batch_exec_fault_n=0
        fi
        if [ "$_batch_exec_fault_n" -ge "${SPIRA_BATCH_EXEC_FAULT_THRESHOLD:-5}" ]; then
            _batch_exec_fault=1
            log "batch: harness fault — $_batch_exec_fault_n consecutive exec failures (podman exec not reaching suites)"
            # Reclassify any red+0s+empty results already recorded so they are not
            # treated as branch failures by gate-retry's flake observer.
            for _es_s in $SELECTED; do
                _es_res="$RESULTS/$_es_s.result"
                [ -f "$_es_res" ]                                               || continue
                [ "$(awk '{print $1}' "$_es_res" 2>/dev/null)" = "red" ]       || continue
                [ "$(awk '{print $3}' "$_es_res" 2>/dev/null)" = "0"  ]        || continue
                [ -z "$(cat "$RESULTS/$_es_s.out" 2>/dev/null | tr -d '[:space:]')" ] || continue
                printf 'unreached %s 0 -\n' "$(date +%s)" > "$_es_res"
                : > "$RESULTS/$_es_s.out"
            done
            break
        fi

        # Container-death check with retry: a transient inspect failure is not death.
        _container_check_live
        if [ "$_batch_container_dead" = 1 ]; then
            log "batch: container died during $s (${_batch_container_fault_detail}) — remaining suites will be unreached"
            break
        fi

        # Write the output file before the result file. The result file's presence is
        # the signal that the suite completed; readers must not see it before the output.
        printf '%s\n' "$out" > "$out_file"

        # 77: skip (automake convention; already in suites.sh). Not a failure, not filed.
        # The 6th field is the producer — who decided which suites to run (explicit /
        # diff / all). Same reasoning as the MODE field: a weaker claim must not
        # conflate with a stronger one.
        _result_extra=" $_SELECTION_TYPE"
        _s_quarantined=0
        case " ${_STS_QUARANTINED:-} " in *" $s "*) _s_quarantined=1 ;; esac
        case "$_rc" in
            0)
                printf '%s %s %s %s %s%s 0\n' ok "$(date +%s)" "$secs" - "$MODE" \
                    "$_result_extra" > "$res_file"
                printf '  %-32s ok      %ss\n' "$s" "$secs"
                ;;
            77)
                printf '%s %s %s %s %s%s 77\n' skip "$(date +%s)" "$secs" - "$MODE" \
                    "$_result_extra" > "$res_file"
                printf '  %-32s SKIPPED\n' "$s"
                ;;
            124)
                printf '%s %s %s %s %s%s 124\n' timeout "$(date +%s)" "$secs" "timeout:$s" "$MODE" \
                    "$_result_extra" > "$res_file"
                _batch_red=$(( _batch_red + 1 ))
                printf '  %-32s TIMEOUT after %ss\n' "$s" "$secs"
                ;;
            *)
                _fp_val="$(_fp "$_rc" "$out")"
                if [ "$_s_quarantined" = 1 ]; then
                    printf '%s %s %s %s %s%s %s\n' quarantined-red "$(date +%s)" "$secs" \
                        "$_fp_val" "$MODE" "$_result_extra" "$_rc" > "$res_file"
                    _batch_quarantined_red=$(( _batch_quarantined_red + 1 ))
                    printf '  %-32s QUARANTINED-RED  rc=%s after %ss\n' "$s" "$_rc" "$secs"
                else
                    printf '%s %s %s %s %s%s %s\n' red "$(date +%s)" "$secs" "$_fp_val" "$MODE" \
                        "$_result_extra" "$_rc" > "$res_file"
                    _batch_red=$(( _batch_red + 1 ))
                    printf '  %-32s RED     rc=%s after %ss\n' "$s" "$_rc" "$secs"
                fi
                ;;
        esac
        _s_bd_result="$(_batch_bd_read "$_serial_bd_log")"
        _append_suite_times "$s" "$_rc" "$secs" \
            "${_s_bd_result%%	*}" "${_s_bd_result##*	}" "$MODE"
    done

else

    # Parallel: suites run concurrently, each with its own HOME, SPIRA_INSTANCE,
    # SPIRA_RUN, and private testdb copy (from the shared baseline when available).
    #
    # Each subshell writes .out and .result immediately on completion, so
    # results appear as suites finish — not after all suites complete. The
    # .rc signal file goes to $par_tmp so the main shell can count reds.
    #
    # A suite that passes serially and fails in parallel means shared state
    # leaked between suites — a bead against the leak, never a retry.
    #
    # _maxpar derived above from hardware (or SPIRA_BATCH_MAXPAR override); 0 = unlimited.
    _par_tmp="$(mktemp -d)"
    _par_pids=""
    _n=0

    # Returns 0 (true) when memory PSI avg10 exceeds the configured threshold.
    _psi_above_threshold() {
        local _t="${SPIRA_BATCH_PSI_THRESHOLD:-10}"
        [ "${_t:-0}" -gt 0 ] 2>/dev/null || return 1
        local _v
        _v="$(awk '/^some/{for(i=1;i<=NF;i++) if($i~/^avg10=/){sub(/avg10=/,"",$i);print $i;exit}}' \
            /proc/pressure/memory 2>/dev/null || echo 0)"
        awk -v v="$_v" -v t="$_t" 'BEGIN{exit(v+0>t+0)?0:1}'
    }

    # SCHEDULING ORDER: exclusive suites run first, ahead of the parallel pool.
    # The drain below only pays for suites already in flight — an exclusive suite
    # sitting at its alphabetical position mid-list drains a pool that has had time
    # to fill, so the drain waits out whichever suites started first (sp-rbulh:
    # measured at 13% of a full-corpus gate). Moving exclusive suites to the front
    # means the drain always finds an empty pool: this loop only reorders, the
    # exclusive/non-exclusive selection made below is unaffected.
    # Within the parallel pool the suites are further ordered longest-first (LPT):
    # the slowest suites start first so a long tail never forms behind a pool that
    # has already had time to drain empty (sp-ezkp3, following sp-rbulh above).
    _par_order=""
    for _os in $SELECTED; do
        [ -n "$(suite_exclusive_of "$SUITE_DIR/$_os")" ] && _par_order="$_par_order $_os"
    done
    _par_pool=""
    for _os in $SELECTED; do
        [ -z "$(suite_exclusive_of "$SUITE_DIR/$_os")" ] && _par_pool="$_par_pool $_os"
    done
    _par_order="$_par_order $(_lpt_order "$_par_pool")"
    _par_order="$(echo $_par_order)"

    for s in $_par_order; do
        # Exclusive suites drain all in-flight parallel jobs and run alone — prevents OOM
        # when a heavy suite (e.g. a cargo build) runs alongside others in the same container.
        _excl_reason="$(suite_exclusive_of "$SUITE_DIR/$s")"
        if [ -n "$_excl_reason" ]; then
            for _ep in $_par_pids; do wait "$_ep" 2>/dev/null || true; done
            _par_pids=""
            log "batch: draining for exclusive suite $s (${_excl_reason})"
            _container_check_live
            if [ "$_batch_container_dead" = 1 ]; then
                log "batch: container died before exclusive suite $s — remaining suites will be unreached"
                break
            fi
        fi
        # PSI guard: pause if the guest is under memory pressure.
        while _psi_above_threshold; do
            log "batch: memory pressure avg10 > ${SPIRA_BATCH_PSI_THRESHOLD:-10}% — pausing suite launch"
            sleep 5
        done

        _n=$((_n+1))
        _suite_instance="${INSTANCE}-${_n}"
        _suite_run="/tmp/spira-batch-${INSTANCE}-${_n}"
        _suite_home="/tmp/spira-batch-${INSTANCE}-${_n}/home"
        _t0="$(date +%s)"

        # Throttle: wait for a slot before launching the next suite.
        # `wait -n` (bash 4.3+) waits for exactly one job; the fallback
        # loop with `true` prevents a hard failure on older bash.
        if [ -z "$_excl_reason" ] && [ "${_maxpar:-0}" -gt 0 ] 2>/dev/null; then
            while [ "$(jobs -rp | wc -l)" -ge "$_maxpar" ]; do
                wait -n 2>/dev/null || true
            done
        fi

        # Create the per-suite home dir inside the container before the subshell
        # starts. The suite then runs with HOME pointing here; $HOME/.config and
        # $HOME/.local are clean per-suite scratch that no other suite can see.
        podman exec --user "$_SPIRA_USER" "$CNAME" \
            mkdir -p "${_suite_home}/.config/systemd/user" \
            >/dev/null 2>&1 || true

        # Each variable is captured by value at fork time; the outer loop
        # changes them, but each subshell has the snapshot from this iteration.
        (
            _par_bd_log="${_suite_run}/bd-calls.log"
            _inner_rc=0
            if [ "${_suite_timeout:-0}" -gt 0 ] 2>/dev/null; then
                timeout "$_suite_timeout" podman exec --user "$_SPIRA_USER" \
                    -e "HOME=${_suite_home}" \
                    -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
                    -e "DBUS_SESSION_BUS_ADDRESS=unix:path=${_USER_RUNTIME}/bus" \
                    -e "CARGO_HOME=${_CONTAINER_CARGO}" \
                    -e "CARGO_TARGET_DIR=${_CONTAINER_CARGO_TARGET}" \
                    -e "SPIRA_IN_TESTENV=1" \
                    "${_testdb_env[@]}" \
                    -e "TMUX=" \
                    -e "SPIRA_PATH=${_shim_spira_path}" \
                    -e "SPIRA_BD_LOG=${_par_bd_log}" \
                    -e "SPIRA_INSTANCE=${_suite_instance}" \
                    -e "SPIRA_RUN=${_suite_run}" \
                    -e "SPIRA_TESTDB_DATA=/tmp/spira-batch-${INSTANCE}/testdb" \
                    -e "SPIRA_BD_TIMING_LOG=${_bd_timing_dir}/${s}.log" \
                    "$CNAME" bash "${_CONTAINER_WORKSPACE}/spira/$s" \
                    >"$_par_tmp/$s.rawout" 2>&1 || _inner_rc=$?
            else
                podman exec --user "$_SPIRA_USER" \
                    -e "HOME=${_suite_home}" \
                    -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
                    -e "DBUS_SESSION_BUS_ADDRESS=unix:path=${_USER_RUNTIME}/bus" \
                    -e "CARGO_HOME=${_CONTAINER_CARGO}" \
                    -e "CARGO_TARGET_DIR=${_CONTAINER_CARGO_TARGET}" \
                    -e "SPIRA_IN_TESTENV=1" \
                    "${_testdb_env[@]}" \
                    -e "TMUX=" \
                    -e "SPIRA_PATH=${_shim_spira_path}" \
                    -e "SPIRA_BD_LOG=${_par_bd_log}" \
                    -e "SPIRA_INSTANCE=${_suite_instance}" \
                    -e "SPIRA_RUN=${_suite_run}" \
                    -e "SPIRA_TESTDB_DATA=/tmp/spira-batch-${INSTANCE}/testdb" \
                    -e "SPIRA_BD_TIMING_LOG=${_bd_timing_dir}/${s}.log" \
                    "$CNAME" bash "${_CONTAINER_WORKSPACE}/spira/$s" \
                    >"$_par_tmp/$s.rawout" 2>&1 || _inner_rc=$?
            fi

            _secs=$(( $(date +%s) - _t0 ))
            _out="$(cat "$_par_tmp/$s.rawout" 2>/dev/null || true)"

            # Write output before result — same ordering guarantee as serial.
            printf '%s\n' "$_out" > "$RESULTS/$s.out"

            # _SELECTION_TYPE and _STS_QUARANTINED are captured by value at fork time.
            _par_extra=" $_SELECTION_TYPE"
            _par_quarantined=0
            case " ${_STS_QUARANTINED:-} " in *" $s "*) _par_quarantined=1 ;; esac
            case "$_inner_rc" in
                0)
                    printf '%s %s %s %s %s%s 0\n' ok "$(date +%s)" "$_secs" - "$MODE" \
                        "$_par_extra" > "$RESULTS/$s.result"
                    printf '  %-32s ok      %ss\n' "$s" "$_secs"
                    ;;
                77)
                    printf '%s %s %s %s %s%s 77\n' skip "$(date +%s)" "$_secs" - "$MODE" \
                        "$_par_extra" > "$RESULTS/$s.result"
                    printf '  %-32s SKIPPED\n' "$s"
                    ;;
                124)
                    printf '%s %s %s %s %s%s 124\n' timeout "$(date +%s)" "$_secs" "timeout:$s" "$MODE" \
                        "$_par_extra" > "$RESULTS/$s.result"
                    printf '  %-32s TIMEOUT after %ss\n' "$s" "$_secs"
                    ;;
                *)
                    _fp_val="$(_fp "$_inner_rc" "$_out")"
                    if [ "$_par_quarantined" = 1 ]; then
                        printf '%s %s %s %s %s%s %s\n' quarantined-red "$(date +%s)" "$_secs" \
                            "$_fp_val" "$MODE" "$_par_extra" "$_inner_rc" > "$RESULTS/$s.result"
                        printf '  %-32s QUARANTINED-RED  rc=%s after %ss\n' "$s" "$_inner_rc" "$_secs"
                    else
                        printf '%s %s %s %s %s%s %s\n' red "$(date +%s)" "$_secs" "$_fp_val" "$MODE" \
                            "$_par_extra" "$_inner_rc" > "$RESULTS/$s.result"
                        printf '  %-32s RED     rc=%s after %ss\n' "$s" "$_inner_rc" "$_secs"
                    fi
                    ;;
            esac

            _p_bd_result="$(_batch_bd_read "$_par_bd_log")"
            _append_suite_times "$s" "$_inner_rc" "$_secs" \
                "${_p_bd_result%%	*}" "${_p_bd_result##*	}" "$MODE"

            # Signal to the main shell that this suite completed and its rc.
            printf '%s\n' "$_inner_rc" > "$_par_tmp/$s.rc"
        ) &
        if [ -n "${_excl_reason:-}" ]; then
            # Exclusive: wait here so no other suite starts until this one finishes.
            wait "$!" 2>/dev/null || true
        else
            _par_pids="$_par_pids $!"
        fi
    done

    # Wait for any remaining in-flight jobs (non-exclusive suites from the last batch).
    for _pid in $_par_pids; do
        wait "$_pid" 2>/dev/null || true
    done

    # Container-death check with retry after all jobs have finished.
    _container_check_live
    if [ "$_batch_container_dead" = 1 ]; then
        log "batch: container died during parallel run (${_batch_container_fault_detail})"
    else
        # Exec-storm detection: only relevant when the container is alive. K or more
        # suites returning red+0s+empty-output means podman exec was refusing rather
        # than suites failing legitimately — a harness fault, not a branch fault.
        _par_exec_fault_n=0
        for _ef_s in $SELECTED; do
            _ef_res="$RESULTS/$_ef_s.result"
            [ -f "$_ef_res" ]                                               || continue
            [ "$(awk '{print $1}' "$_ef_res" 2>/dev/null)" = "red" ]       || continue
            [ "$(awk '{print $3}' "$_ef_res" 2>/dev/null)" = "0"  ]        || continue
            [ -z "$(cat "$RESULTS/$_ef_s.out" 2>/dev/null | tr -d '[:space:]')" ] || continue
            _par_exec_fault_n=$((_par_exec_fault_n + 1))
        done
        if [ "$_par_exec_fault_n" -ge "${SPIRA_BATCH_EXEC_FAULT_THRESHOLD:-5}" ]; then
            _batch_exec_fault=1
            log "batch: harness fault — $_par_exec_fault_n parallel exec failures (podman exec not reaching suites)"
        fi
    fi

    # Reclassify red+0s+empty-output suites as unreached when the container died or
    # exec-storm was detected: podman exec failed rather than the suite running and
    # failing. Recording them as red feeds gate-retry's flake observer and quarantines
    # healthy suites; unreached lets the retry logic skip them.
    if [ "$_batch_container_dead" = 1 ] || [ "$_batch_exec_fault" = 1 ]; then
        for _cd_s in $SELECTED; do
            _cd_res="$RESULTS/$_cd_s.result"
            [ -f "$_cd_res" ]                                           || continue
            _cd_status="$(awk '{print $1}' "$_cd_res" 2>/dev/null)"
            [ "$_cd_status" = "red" ]                                   || continue
            _cd_secs="$(awk '{print $3}' "$_cd_res" 2>/dev/null)"
            [ "${_cd_secs:-1}" = "0" ]                                  || continue
            _cd_out="$(cat "$RESULTS/$_cd_s.out" 2>/dev/null | tr -d '[:space:]')"
            [ -z "$_cd_out" ]                                           || continue
            printf 'unreached %s 0 -\n' "$(date +%s)" > "$_cd_res"
            : > "$RESULTS/$_cd_s.out"
            rm -f "$_par_tmp/$_cd_s.rc"
        done
    fi

    # Count reds from the signal files. Suites with no .rc file were never
    # reached (e.g., the bash process was killed before all subshells launched);
    # the unreached loop below handles those. Quarantined suites are not counted
    # as blocking reds (their result file says quarantined-red).
    for s in $SELECTED; do
        [ -f "$_par_tmp/$s.rc" ] || continue
        _par_rc="$(cat "$_par_tmp/$s.rc")"
        case "$_par_rc" in
            0|77) ;;
            *)
                case " ${_STS_QUARANTINED:-} " in
                    *" $s "*)
                        _batch_quarantined_red=$((_batch_quarantined_red+1)) ;;
                    *) _batch_red=$((_batch_red+1)) ;;
                esac ;;
        esac
    done

    rm -rf "$_par_tmp"
    _par_tmp=""

    # Log the container's cgroup peak memory so the per-suite budget is measured,
    # not guessed, on every run. Divide by maxpar for an estimate per slot.
    _cg_path="$(podman inspect --format '{{.State.CgroupPath}}' "$CNAME" 2>/dev/null || true)"
    if [ -n "${_cg_path:-}" ]; then
        _peak_bytes="$(cat "/sys/fs/cgroup/${_cg_path#/}/memory.peak" 2>/dev/null || true)"
        if [ -n "${_peak_bytes:-}" ] && [ "${_peak_bytes}" -gt 0 ] 2>/dev/null; then
            _peak_mib=$(( _peak_bytes / 1048576 ))
            _per_slot_mib=$(( _maxpar > 0 ? _peak_mib / _maxpar : _peak_mib ))
            log "batch: cgroup peak ${_peak_mib}MiB (maxpar ${_maxpar:-?}, ~${_per_slot_mib}MiB/slot; budget ${_mem_per_suite_mib:-192}MiB/suite)"
        fi
    fi

fi

_suites_wall=$(( $(date +%s) - _suites_t0 ))
_timing_cpu1="$(awk '/^cpu /{s=0;for(i=2;i<=NF;i++)s+=$i;idle=$6+$7;printf "%d %d",s,idle;exit}' \
    /proc/stat 2>/dev/null || printf '0 0')"

rm -f "$_batch_tmp"

# ---------------------------------------------------------------------------
# UNREACHED — any selected suite with no result file was not reached.
# Do not overwrite a completed status — that is the sp-u1g defect exactly.
# ---------------------------------------------------------------------------
for s in $SELECTED; do
    res_file="$RESULTS/$s.result"
    [ -f "$res_file" ] && continue  # completed — never overwrite
    out_file="$RESULTS/$s.out"
    printf 'unreached %s 0 -\n' "$(date +%s)" > "$res_file"
    : > "$out_file"
    printf '  %-32s UNREACHED\n' "$s"
done

# ---------------------------------------------------------------------------
# BATCH METADATA — image tag and key in one file so the result is self-contained.
# Readers who want to know what image produced this run do not have to infer it.
# ---------------------------------------------------------------------------
printf 'image_tag=%s\nbranch=%s\nbase=%s\nkey=%s\nmode=%s\nselection=%s\n' \
    "$IMG_TAG" "$BR" "$BASE" "${BATCH_KEY:--}" "$MODE" "$_SELECTION_TYPE" \
    > "$RESULTS/batch.meta"

# ---------------------------------------------------------------------------
# SUITE-TIMES BATCH SUMMARY — one row per batch with the end-to-end wall time.
# Suite name __batch__ is reserved for this row; tsd-query.sh's last-run and
# suite-medians both treat it as any other suite value, and callers filter it out.
# ---------------------------------------------------------------------------
_BATCH_WALL=$(( $(date +%s) - _BATCH_T0 ))
_tsd_suite_timing "__batch__" "0" "$_BATCH_WALL" "0" "0" "$MODE"
log "batch: wall ${_BATCH_WALL}s"

# ---------------------------------------------------------------------------
# RUNNER METADATA — machine shape captured alongside batch.meta.
# ---------------------------------------------------------------------------
{
    _cpu_pct="-"
    _c0t="$(printf '%s' "$_timing_cpu0" | awk '{print $1}')"
    _c0i="$(printf '%s' "$_timing_cpu0" | awk '{print $2}')"
    _c1t="$(printf '%s' "$_timing_cpu1" | awk '{print $1}')"
    _c1i="$(printf '%s' "$_timing_cpu1" | awk '{print $2}')"
    _dt=$(( _c1t - _c0t )); _di=$(( _c1i - _c0i ))
    [ "$_dt" -gt 0 ] && _cpu_pct=$(( 100 * (_dt - _di) / _dt )) || true
    printf 'nproc=%s\nmemtotal_kb=%s\nmaxpar=%s\ncpu_busy_pct=%s\nsuites_wall_s=%s\n' \
        "$_timing_nproc" "$_timing_memtotal_kb" \
        "${SPIRA_BATCH_MAXPAR:-$(nproc 2>/dev/null || printf '-')}" \
        "$_cpu_pct" "$_suites_wall" \
        > "$RESULTS/runner.meta"
} 2>/dev/null || true

# ---------------------------------------------------------------------------
# TIMING.TSV — per-suite name, wall_s, result, bd_ms.
# bd_ms is total milliseconds spent in bd calls (from container shim logs);
# "-" when the shim was not active or produced no log for that suite.
# ---------------------------------------------------------------------------
_bd_timing_raw=""
_bd_timing_raw="$(
    podman exec --user "$_SPIRA_USER" "$CNAME" bash -c "
        for f in \"${_bd_timing_dir}\"/*.log; do
            [ -f \"\$f\" ] || continue
            suite=\"\$(basename \"\$f\" .log)\"
            ms=\"\$(awk '{s+=\$1} END{print s+0}' \"\$f\" 2>/dev/null || printf 0)\"
            printf '%s\t%s\n' \"\$suite\" \"\$ms\"
        done
    " 2>/dev/null
)" 2>/dev/null || true
{
    for _ts in $SELECTED; do
        [ -f "$RESULTS/$_ts.result" ] || continue
        _ts_wall="$(awk '{print $3}' "$RESULTS/$_ts.result" 2>/dev/null)"
        case "${_ts_wall:-x}" in ''|*[!0-9]*) _ts_wall="-" ;; esac
        _ts_st="$(awk '{print $1}' "$RESULTS/$_ts.result" 2>/dev/null)"
        [ -n "${_ts_st:-}" ] || _ts_st="-"
        _ts_bd="-"
        if [ -n "${_bd_timing_raw:-}" ]; then
            _ts_bd_v="$(printf '%s\n' "$_bd_timing_raw" | \
                awk -F'\t' -v s="$_ts" '$1==s{print $2; exit}')"
            case "${_ts_bd_v:-}" in ''|*[!0-9]*) ;; *) _ts_bd="$_ts_bd_v" ;; esac
        fi
        printf '%s\t%s\t%s\t%s\n' "$_ts" "$_ts_wall" "$_ts_st" "$_ts_bd"
    done
} > "$RESULTS/timing.tsv" 2>/dev/null || true

# ---------------------------------------------------------------------------
# VERDICT — three distinguishable outcomes.
# ---------------------------------------------------------------------------
if [ "$_TESTDB_BATCH_SHARED" = 1 ]; then
    log "batch: testdb: shared baseline used (mode: ${_TESTDB_SHARED_MODE:-?}, name: ${_TESTDB_SHARED_NAME})"
fi

if [ "$_batch_exec_fault" = 1 ]; then
    log "batch: harness fault — exec failures, not suite reds"
    exit 2
fi

if [ "$_batch_container_dead" = 1 ]; then
    log "batch: harness fault — container died mid-batch (${_batch_container_fault_detail})"
    exit 2
fi

if [ "$_batch_quarantined_red" -gt 0 ]; then
    _qr_names=""
    for s in ${_STS_QUARANTINED:-}; do
        [ -r "$RESULTS/$s.result" ] || continue
        case "$(awk '{print $1}' "$RESULTS/$s.result" 2>/dev/null)" in
            quarantined-red) _qr_names="${_qr_names:+$_qr_names, }$s" ;;
        esac
    done
    log "batch: quarantined red (not blocking): ${_qr_names:-$_batch_quarantined_red suite(s)}"
fi

if [ "$_batch_red" -gt 0 ]; then
    log "batch: $_batch_red suite(s) red"
    _diag="$HERE/gate-diag.sh"
    [ -r "$_diag" ] && bash "$_diag" "$RESULTS" || true
    if [ -n "$BATCH_KEY" ] && [ "$verdict_ttl" -gt 0 ]; then
        mkdir -p "$VERDICT_DIR"
        _red_names=""
        for _rv_s in $SELECTED; do
            [ -r "$RESULTS/$_rv_s.result" ] || continue
            case "$(awk '{print $1}' "$RESULTS/$_rv_s.result" 2>/dev/null)" in
                red|timeout) _red_names="${_red_names:+$_red_names }$_rv_s" ;;
            esac
        done
        printf 'verdict=red\nwhen=%s\nby=%s\nat=%s\nred_suites=%s\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "testenv-batch" "$(date +%s)" \
            "${_red_names:-unknown}" \
            > "$VERDICT_DIR/batch-$BATCH_KEY"
        [ -n "${_verdict_override_reason:-}" ] && \
            printf 'override_reason=%s\n' "$_verdict_override_reason" \
                >> "$VERDICT_DIR/batch-$BATCH_KEY"
    fi
    [ -r "$HERE/gate-timing.sh" ] && bash "$HERE/gate-timing.sh" "$RESULTS" red 2>/dev/null || true
    exit 1
fi

# All suites passed or skipped — cache the verdict so the same tree skips next time.
if [ -n "$BATCH_KEY" ] && [ "$verdict_ttl" -gt 0 ]; then
    mkdir -p "$VERDICT_DIR"
    printf 'verdict=green\nwhen=%s\nby=%s\nat=%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "testenv-batch" "$(date +%s)" \
        > "$VERDICT_DIR/batch-$BATCH_KEY"
    [ -n "${_verdict_override_reason:-}" ] && \
        printf 'override_reason=%s\n' "$_verdict_override_reason" \
            >> "$VERDICT_DIR/batch-$BATCH_KEY"
fi

log "batch: all suites passed"
[ -r "$HERE/gate-timing.sh" ] && bash "$HERE/gate-timing.sh" "$RESULTS" green 2>/dev/null || true
exit 0
