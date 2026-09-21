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
#                    [--report [N]]
#                    <branch> [<repo-name-or-path>]
#
#   --suites -  reads suite names from stdin, one per line (blank lines ignored).
#               Empty stdin means "nothing to run" — exit 0, not an error.
#               Composes with a selector:
#                 select.sh --base X --head Y | testenv-batch.sh --suites - <branch> [<repo>]
#   --report N  print the top-20 report over the last N runs from the suite-times ledger;
#               no branch or container is required. Delegates to suite-times.sh.
#
# EXIT STATUS
#   0   all selected suites passed or skipped
#   1   suites ran, some were red
#   2   container did not come up, or died mid-batch (harness fault — not the branch)
#   3   install inside the container failed (harness fault — not the branch)
#
# ENVIRONMENT (all optional)
#   SPIRA_BATCH_RESULTS     host root for result directories
#                           (default: SPIRA_RUN/batch-results)
#   SPIRA_BATCH_INSTANCE    container instance name; determines CNAME and the
#                           install instance; default: first 12 chars of the batch key
#   SPIRA_BATCH_SUITE_DIR   where to look for test-*.sh on the host
#                           (default: the spira/ directory beside this script)
#   SPIRA_BATCH_SKIP_INSTALL  if non-empty, skip configure+install; suites that
#                             need installed units will skip (exit 77)
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
#   SPIRA_BATCH_MAXPAR      max parallel suites in --mode parallel.
#                           Default: nproc (quota-aware: honours CPUQuota of the calling
#                           unit, not the host's physical count; this is intentional — the
#                           suite pool runs within the unit's own allocation). CPU is the
#                           binding resource: on a 4-core host, 37 concurrent suites drove
#                           CPU pressure to 97% and produced fork-EAGAIN errors the gate
#                           blamed on the branch rather than the load. The PID budget
#                           (container pids-limit 8 192) is a ceiling, not the sizing
#                           input — at nproc=4 the peak is far below it. Set to 0 for
#                           unlimited (useful for small explicit selections or stress tests).

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"
. "$HERE/suite-covers.sh"
. "$HERE/suite-state.sh"
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
_BATCH_REPORT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --mode)
            [ $# -ge 2 ] || { printf 'batch: --mode requires an argument\n' >&2; exit 2; }
            MODE="$2"; shift 2 ;;
        --mode=*)
            MODE="${1#--mode=}"; shift ;;
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
            _BATCH_REPORT="${2:-2}"
            case "$_BATCH_REPORT" in [0-9]*) shift 2 ;; *) _BATCH_REPORT=2; shift ;; esac ;;
        --report=*)
            _BATCH_REPORT="${1#--report=}"; shift ;;
        --)
            shift; break ;;
        -*)
            printf 'batch: unknown option: %s\n' "$1" >&2
            printf 'usage: testenv-batch.sh [--mode parallel|serial] [--suites <list|->] [--report [N]] <branch> [<repo-name>]\n' >&2
            exit 2 ;;
        *)  break ;;
    esac
done

# --report: delegate to suite-times.sh and exit; no branch or container needed.
if [ -n "$_BATCH_REPORT" ]; then
    exec bash "$HERE/suite-times.sh" report "$_BATCH_REPORT" "$@"
fi
case "$MODE" in
    parallel|serial) ;;
    *) printf 'batch: --mode must be parallel or serial, got: %s\n' "$MODE" >&2; exit 2 ;;
esac

BR="${1:-}"
REPO_ARG="${2:-}"
[ -n "$BR" ] || {
    printf 'usage: testenv-batch.sh [--mode parallel|serial] <branch> [<repo-name>]\n' >&2
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
    printf '%s\n' "$REPO_NAME $tree $IMG_TAG $sel_h $harness_h $MODE $_SELECTION_TYPE" | sha256sum | cut -d' ' -f1
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
                        } | SPIRA_INCIDENT_REF="repeat-refused:$BR" \
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

_batch_cleanup() {
    bash "$TESTENV" down --name "$CNAME" >/dev/null 2>&1 || true
    rm -f "$_BATCH_OWNER_FILE"
    _wt_cleanup
    rm -f "$_batch_tmp"
    [ -n "$_par_tmp" ] && rm -rf "$_par_tmp" || true
}
trap _batch_cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# ORPHAN SWEEP — remove spira-batch-* containers whose owner PID has exited.
# Runs before we start our own container so a crashed previous run does not
# consume memory for the duration of this one.
# ---------------------------------------------------------------------------
for _sw_f in /tmp/spira-batch-*.owner; do
    [ -f "$_sw_f" ] || continue
    _sw_pid="$(cat "$_sw_f" 2>/dev/null)" || continue
    [ -n "$_sw_pid" ] || continue
    [ -d "/proc/$_sw_pid" ] && continue  # still alive
    _sw_cname="${_sw_f#/tmp/}"; _sw_cname="${_sw_cname%.owner}"
    podman stop "$_sw_cname" >/dev/null 2>&1 || true
    podman rm   "$_sw_cname" >/dev/null 2>&1 || true
    rm -f "$_sw_f"
    log "batch: swept orphan container $_sw_cname (owner pid $_sw_pid gone)"
done

printf '%s\n' "$$" > "$_BATCH_OWNER_FILE"

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
# REQUIREMENTS CHECK — suites declaring # requires: tokens are pre-checked
# inside the container. An unmet requirement records skip-req (distinct from
# exit-77 self-skip) with the missing token named in the fingerprint field.
# The suite is removed from the run list, so it is never "unreached".
#
# Each unique token is checked exactly once via `command -v` inside the
# container; the result is cached so a token shared by several suites costs
# one podman exec, not one per suite.
# ---------------------------------------------------------------------------
_req_all_tokens=""
for _rs in $SELECTED; do
    _reqs="$(suite_requires_of "$SUITE_DIR/$_rs")"
    for _tok in $_reqs; do
        [ -n "$_tok" ] || continue
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
#   TESTDB_NAME:    TESTDB_SHARED=0 + empty TESTDB_NAME → testdb.sh generates a
#                   unique name per invocation; multiple parallel calls each get
#                   their own fixture database.
#
# SCAR: test-install-migrate.sh and test-install-instance.sh plant legacy systemd
# unit fixtures (spira-sentinel.service etc.) in the real systemd unit directory via
# `systemctl --user enable`. With per-suite HOME those suites need to either use
# SPIRA_INSTANCE namespacing or declare a skip when HOME is not the real container
# home (XDG_RUNTIME_DIR check already acts as a container guard; HOME can extend it).
# ---------------------------------------------------------------------------
_n_selected=0; for _s in $SELECTED; do _n_selected=$((_n_selected+1)); done

# ---------------------------------------------------------------------------
# RUN ID — identifies this batch in the suite-times ledger.
# GITHUB_RUN_ID is set by GitHub Actions; local runs use a timestamp.
# ---------------------------------------------------------------------------
_BATCH_RUN_ID="${GITHUB_RUN_ID:-${SPIRA_BATCH_RUN_ID:-local-$(date +%s)}}"

# ---------------------------------------------------------------------------
# BD SHIM — a timing wrapper installed ahead of the real bd on SPIRA_PATH.
# The shim appends "<ms> <rc> <subcommand>" to SPIRA_BD_LOG per call, letting
# the batch attribute each suite's wall time to bd versus non-bd work.
# Set SPIRA_BATCH_NO_BD_SHIM=1 to disable (e.g. for skip-install runs that
# have no installed bd or when the container image is stripped).
# ---------------------------------------------------------------------------
_shim_dir="/tmp/spira-batch-${INSTANCE}/shim"
_shim_spira_path=""
if [ -z "${SPIRA_BATCH_NO_BD_SHIM:-}" ]; then
    _real_bd="$(podman exec --user "$_SPIRA_USER" \
        -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
        "$CNAME" bash -c 'command -v bd 2>/dev/null' 2>/dev/null || true)"
    if [ -n "$_real_bd" ]; then
        podman exec --user "$_SPIRA_USER" "$CNAME" mkdir -p "$_shim_dir" \
            >/dev/null 2>&1 || true
        _shim_tmp="$(mktemp)"
        _shim_tmp2="${_shim_tmp}.bd"
        cat > "$_shim_tmp" << 'SHIM_EOF'
#!/usr/bin/env bash
_t0=$(date +%s%3N)
_rc=0; REAL_BD "$@" || _rc=$?
_log="${SPIRA_BD_LOG:-${SPIRA_RUN:-/tmp}/bd-calls.log}"
mkdir -p "${_log%/*}" 2>/dev/null
printf '%s %s %s\n' $(( $(date +%s%3N) - _t0 )) "$_rc" "${1:--}" >> "$_log" 2>/dev/null
exit $_rc
SHIM_EOF
        sed "s|REAL_BD|${_real_bd}|g" "$_shim_tmp" > "$_shim_tmp2"
        if podman cp "$_shim_tmp2" "${CNAME}:${_shim_dir}/bd" >/dev/null 2>&1 \
           && podman exec --user "$_SPIRA_USER" "$CNAME" \
                  chmod +x "${_shim_dir}/bd" >/dev/null 2>&1; then
            _shim_spira_path="$_shim_dir"
            log "batch: bd shim installed (real bd: ${_real_bd})"
        else
            log "batch: bd shim install failed — bd timing will be absent"
        fi
        rm -f "$_shim_tmp" "$_shim_tmp2"
    else
        log "batch: bd not found in container — bd timing absent"
    fi
fi

# _batch_bd_read <log-path-inside-container> — print "<calls>\t<ms>"
_batch_bd_read() {
    podman exec --user "$_SPIRA_USER" \
        -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
        "$CNAME" awk 'BEGIN{n=0;s=0}{n++;s+=$1}END{printf "%d\t%d",n,s}' \
        "$1" 2>/dev/null || printf '0\t0'
}

# _append_suite_times <suite> <rc> <wall_secs> <bd_calls> <bd_ms> <mode>
_append_suite_times() {
    local _row
    _row="$(printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s' \
        "$_BATCH_RUN_ID" "$BR" "$1" "$2" "$3" "$4" "$5" "$6")"
    printf '%s\n' "$_row" >> "$RESULTS/suite-times.tsv"
    mkdir -p "${SPIRA_SUITE_TIMES_LOG%/*}" 2>/dev/null || true
    printf '%s\n' "$_row" >> "${SPIRA_SUITE_TIMES_LOG:-$SPIRA_RUN/suite-times.log}" 2>/dev/null || true
}

if [ "$MODE" = parallel ]; then
    _maxpar_display="${SPIRA_BATCH_MAXPAR:-$(nproc)}"
    [ "${_maxpar_display:-0}" -gt 0 ] 2>/dev/null \
        && log "batch: running $_n_selected suite(s) in $CNAME (mode: $MODE, maxpar: $_maxpar_display, nproc: $(nproc))" \
        || log "batch: running $_n_selected suite(s) in $CNAME (mode: $MODE, maxpar: unlimited, nproc: $(nproc))"
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
                -e "TESTDB_SHARED=0" \
                -e "TESTDB_NAME=" \
                -e "TESTDB_DIR=" \
                -e "TMUX=" \
                -e "SPIRA_PATH=${_shim_spira_path}" \
                -e "SPIRA_BD_LOG=${_serial_bd_log}" \
                -e "SPIRA_RUN=/tmp/spira-batch-${INSTANCE}" \
                -e "SPIRA_TESTDB_DATA=/tmp/spira-batch-${INSTANCE}/testdb" \
                "$CNAME" bash "${_CONTAINER_WORKSPACE}/spira/$s" >"$_batch_tmp" 2>&1 || _rc=$?
        else
            podman exec --user "$_SPIRA_USER" \
                -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
                -e "DBUS_SESSION_BUS_ADDRESS=unix:path=${_USER_RUNTIME}/bus" \
                -e "CARGO_HOME=${_CONTAINER_CARGO}" \
                -e "CARGO_TARGET_DIR=${_CONTAINER_CARGO_TARGET}" \
                -e "SPIRA_IN_TESTENV=1" \
                -e "TESTDB_SHARED=0" \
                -e "TESTDB_NAME=" \
                -e "TESTDB_DIR=" \
                -e "TMUX=" \
                -e "SPIRA_PATH=${_shim_spira_path}" \
                -e "SPIRA_BD_LOG=${_serial_bd_log}" \
                -e "SPIRA_RUN=/tmp/spira-batch-${INSTANCE}" \
                -e "SPIRA_TESTDB_DATA=/tmp/spira-batch-${INSTANCE}/testdb" \
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
    # SPIRA_RUN, and testdb fixture (TESTDB_SHARED=0 + empty TESTDB_NAME).
    #
    # Each subshell writes .out and .result immediately on completion, so
    # results appear as suites finish — not after all suites complete. The
    # .rc signal file goes to $par_tmp so the main shell can count reds.
    #
    # A suite that passes serially and fails in parallel means shared state
    # leaked between suites — a bead against the leak, never a retry.
    #
    # MAXPAR: SPIRA_BATCH_MAXPAR caps concurrent suites. Default from nproc: the PID
    # budget (container pids-limit) has headroom; CPU is the binding resource.
    _maxpar="${SPIRA_BATCH_MAXPAR:-$(nproc)}"
    _par_tmp="$(mktemp -d)"
    _par_pids=""
    _n=0

    for s in $SELECTED; do
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
                    -e "TESTDB_SHARED=0" \
                    -e "TESTDB_NAME=" \
                    -e "TESTDB_DIR=" \
                    -e "TMUX=" \
                    -e "SPIRA_PATH=${_shim_spira_path}" \
                    -e "SPIRA_BD_LOG=${_par_bd_log}" \
                    -e "SPIRA_INSTANCE=${_suite_instance}" \
                    -e "SPIRA_RUN=${_suite_run}" \
                    -e "SPIRA_TESTDB_DATA=/tmp/spira-batch-${INSTANCE}/testdb" \
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
                    -e "TESTDB_SHARED=0" \
                    -e "TESTDB_NAME=" \
                    -e "TESTDB_DIR=" \
                    -e "TMUX=" \
                    -e "SPIRA_PATH=${_shim_spira_path}" \
                    -e "SPIRA_BD_LOG=${_par_bd_log}" \
                    -e "SPIRA_INSTANCE=${_suite_instance}" \
                    -e "SPIRA_RUN=${_suite_run}" \
                    -e "SPIRA_TESTDB_DATA=/tmp/spira-batch-${INSTANCE}/testdb" \
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

fi

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
# Suite name __batch__ is reserved for this row; suite-times.sh skips it in
# per-suite output but uses it for the "wall" figure in per-run summaries.
# ---------------------------------------------------------------------------
_BATCH_WALL=$(( $(date +%s) - _BATCH_T0 ))
_batch_times_row="$(printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s' \
    "$_BATCH_RUN_ID" "$BR" "__batch__" "0" "$_BATCH_WALL" "0" "0" "$MODE")"
printf '%s\n' "$_batch_times_row" >> "$RESULTS/suite-times.tsv"
mkdir -p "${SPIRA_SUITE_TIMES_LOG%/*}" 2>/dev/null || true
printf '%s\n' "$_batch_times_row" >> \
    "${SPIRA_SUITE_TIMES_LOG:-$SPIRA_RUN/suite-times.log}" 2>/dev/null || true
log "batch: wall ${_BATCH_WALL}s"

# ---------------------------------------------------------------------------
# VERDICT — three distinguishable outcomes.
# ---------------------------------------------------------------------------
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
exit 0
