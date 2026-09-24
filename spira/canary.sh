#!/usr/bin/env bash
#
# canary.sh — end-to-end pipeline canary on an isolated stage.
#
#   canary.sh                   stand up a stage, run, tear down
#   canary.sh --stage <root>    use an already-running stage (skip up/down)
#
# Files a synthetic bead on the stage, runs the REAL sentinel (which dispatches
# the canary worker via SPIRA_SUMMON), runs the REAL landing pass directly, and
# asserts that a commit naming the bead id appears on origin/main inside a
# deadline. On failure it files a spira,incident bead in the PRODUCTION database
# (SPIRA_DB before the stage eval), then exits non-zero.
#
# THE ONE THING THAT IS FAKE IS THE MODEL. SPIRA_SUMMON points to
# fake-summon.sh, which runs canary-worker.sh instead of aeon.sh — a scripted
# worker that claims, commits, and closes the bead without invoking Claude.
# SPIRA_LAUNCH records the landing dispatch and exits 0; canary.sh drives
# landing.sh directly so it controls the timing.
#
# NOTHING TOUCHES THE REAL INSTANCE. STAGE_ROOT contains the stage's own
# SPIRA_DB, SPIRA_RUN, SPIRA_REPO, and SPIRA_HOME. The production vars are
# saved before the eval and restored for the incident filing path.
#
# covers: spira/canary.sh spira/stage.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

DEADLINE="${CANARY_DEADLINE:-120}"   # seconds: how long before we declare failure

_log() { printf '%s canary: %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
_die() { _log "FATAL: $*" >&2; exit 1; }

# ─── parse args ──────────────────────────────────────────────────────────────
EXTERNAL_STAGE=""
case "${1:-}" in
    --stage)
        EXTERNAL_STAGE="${2:?--stage requires a STAGE_ROOT argument}"
        shift 2
        ;;
    "") ;;
    *) printf 'usage: canary.sh [--stage <root>]\n' >&2; exit 1 ;;
esac

# Save production env before the stage eval may override it.
_PROD_DB="${SPIRA_DB:-}"
_PROD_RUN="${SPIRA_RUN:-}"
_PROD_HOME="${SPIRA_HOME:-}"
_PROD_NOTIFY="${SPIRA_NOTIFY:-}"
_PROD_BD="${SPIRA_BD:-bd}"
_PROD_PATH="$PATH"

# ─── incident helper (writes to PRODUCTION, not the stage) ───────────────────
_file_incident() {
    local title="$1" body="$2"
    local inc="$HERE/incident.sh"
    [ -r "$inc" ] || return 0
    [ -n "$_PROD_DB" ] || return 0
    SPIRA_DB="$_PROD_DB" SPIRA_RUN="${_PROD_RUN:-/tmp/canary-inc-$$}" \
    SPIRA_HOME="$_PROD_HOME" SPIRA_NOTIFY="${_PROD_NOTIFY:-/bin/true}" \
    SPIRA_BD="$_PROD_BD" SPIRA_PATH="${_PROD_PATH}" \
    SPIRA_INCIDENT_TYPE=bug \
    SPIRA_INCIDENT_PRIORITY=1 \
    SPIRA_INCIDENT_LABELS="spira,incident" \
    SPIRA_INCIDENT_REPO="${SPIRA_HOME_REPO:-spira}" \
    SPIRA_INCIDENT_REF="canary:pipeline" \
    SPIRA_INCIDENT_CAUSE=canary-fail \
        bash "$inc" file "$title" - <<< "$body" >/dev/null 2>&1 || true
}

# ─── stage setup ─────────────────────────────────────────────────────────────
_t0="$(date +%s)"

if [ -n "$EXTERNAL_STAGE" ]; then
    # Caller already did eval "$(stage.sh up)"; env vars are in scope. Validate.
    [ -f "$EXTERNAL_STAGE/spira/chamber/canary.fayth" ] \
        || _die "--stage $EXTERNAL_STAGE does not look like a stage"
    # Honour STAGE_ROOT from the caller's env; it must agree with the arg.
    STAGE_ROOT="${STAGE_ROOT:-$EXTERNAL_STAGE}"
    _OWN_STAGE=0
else
    eval "$(bash "$HERE/stage.sh" up)" \
        || _die "could not set up stage"
    _OWN_STAGE=1
fi
_log "stage: STAGE_ROOT=$STAGE_ROOT"

# Guaranteed cleanup regardless of how we exit.
_cleanup() {
    local rc=$?
    if [ "${_OWN_STAGE:-0}" = 1 ] && [ -n "${STAGE_ROOT:-}" ]; then
        bash "$HERE/stage.sh" down "$STAGE_ROOT" 2>/dev/null || true
    fi
    exit $rc
}
trap _cleanup EXIT INT TERM

# ─── verify stage isolation --------------------------------------------------
# Refuse to proceed if any stage path resolves outside STAGE_ROOT. A
# misconfiguration here could corrupt a real instance.
for _p in "$SPIRA_DB" "$SPIRA_RUN" "$SPIRA_HOME"; do
    case "$_p" in
        "$STAGE_ROOT"/*|"$STAGE_ROOT") ;;
        *) _die "isolation check failed: $_p is outside STAGE_ROOT=$STAGE_ROOT" ;;
    esac
done
unset _p

# ─── create goal epic and plan bead in the STAGE database ────────────────────
# The goal epic gives sentinel a $SPIRA_GOAL anchor; the plan bead is the work
# the canary traces through the pipeline. The bead has no repo: label, so it
# defaults to the home repo (resolved from SPIRA_REPO via spira_home_repo).
_goal_id="$(bd -C "$SPIRA_DB" create "canary: pipeline goal" \
    --type epic --silent 2>/dev/null | tr -d '[:space:]')"
[ -n "$_goal_id" ] || _die "could not create goal epic in stage db"
_log "goal: $_goal_id"

export SPIRA_GOAL="$_goal_id"

_bead_id="$(bd -C "$SPIRA_DB" create "canary: synthetic pipeline test" \
    --type task --parent "$_goal_id" --labels "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" --silent 2>/dev/null \
    | tr -d '[:space:]')"
[ -n "$_bead_id" ] || _die "could not create plan bead"
_log "bead: $_bead_id"

# ─── run the real sentinel (one pass) ────────────────────────────────────────
# CHECK 6: fake-launch.sh records the dispatch timestamp and exits 0.
# CHECK 7: fake-summon.sh runs canary-worker.sh synchronously, which claims
#          the bead, commits to its branch, and closes it.
_log "running sentinel (one pass)"
SPIRA_HOME="$SPIRA_HOME" SPIRA_RUN="$SPIRA_RUN" SPIRA_DB="$SPIRA_DB" \
SPIRA_REPO="$SPIRA_REPO" SPIRA_REPO_MAP="$SPIRA_REPO_MAP" \
SPIRA_FAYTHS="$SPIRA_FAYTHS" SPIRA_MAX_AEONS="$SPIRA_MAX_AEONS" \
SPIRA_GOAL="$SPIRA_GOAL" \
SPIRA_SUMMON="$SPIRA_SUMMON" SPIRA_LAUNCH="$SPIRA_LAUNCH" \
SPIRA_NOTIFY="$SPIRA_NOTIFY" SPIRA_BD="$SPIRA_BD" \
    bash "$SPIRA_HOME/sentinel.sh" 2>&1 | sed 's/^/  sentinel: /' || true

# Verify the worker closed the bead.
_bead_st="$(bd -C "$SPIRA_DB" show "$_bead_id" --json 2>/dev/null \
    | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
d = d if isinstance(d, list) else [d]
print(d[0].get("status", "") if d else "")' 2>/dev/null)"
if [ "$_bead_st" != "closed" ]; then
    _msg="sentinel pass did not close the bead: status=$_bead_st"
    _log "FAIL: $_msg"
    _file_incident "canary: pipeline bead not closed" \
        "The sentinel pass completed but the bead $_bead_id is $_bead_st.\n\nStage: $STAGE_ROOT"
    exit 1
fi
_log "bead closed: $_bead_id"

# ─── run the real landing pass ───────────────────────────────────────────────
_log "running landing pass"
SPIRA_HOME="$SPIRA_HOME" SPIRA_RUN="$SPIRA_RUN" SPIRA_DB="$SPIRA_DB" \
SPIRA_REPO="$SPIRA_REPO" SPIRA_REPO_MAP="$SPIRA_REPO_MAP" \
SPIRA_BD="$SPIRA_BD" SPIRA_NOTIFY="$SPIRA_NOTIFY" \
    bash "$SPIRA_HOME/landing.sh" 2>&1 | sed 's/^/  landing: /' || true

# ─── assert commit on origin/main --------------------------------------------
# The bead id must appear in a commit subject on the remote's main branch.
# Read from the bare remote directly — no fetch needed, no stale tracking ref.
_elapsed=$(( $(date +%s) - _t0 ))
_found="$(git -C "$STAGE_ROOT/remote.git" log main \
    --format='%s' -n "${SPIRA_VERDICT_WINDOW:-400}" 2>/dev/null \
    | grep -F "$_bead_id" | head -1 || true)"

if [ -z "$_found" ]; then
    _msg="commit naming $_bead_id not found on origin/main after ${_elapsed}s"
    _log "FAIL: $_msg"
    _log "remote/main log (last 5):"
    git -C "$STAGE_ROOT/remote.git" log main --oneline -5 2>/dev/null \
        | sed 's/^/  /' || true
    _file_incident "canary: commit not found on base branch" \
        "Stage canary failed: $_msg\n\nbead_id: $_bead_id\nstage: $STAGE_ROOT\n\nlanding.status:\n$(cat "$SPIRA_RUN/landing.status" 2>/dev/null || echo '(none)')"
    exit 1
fi

_elapsed=$(( $(date +%s) - _t0 ))
_log "PASS — commit '$_found' on origin/main in ${_elapsed}s"
