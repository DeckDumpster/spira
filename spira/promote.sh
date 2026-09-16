#!/usr/bin/env bash
#
# promote.sh — fast-forward the production checkout to a ref; restart only changed units.
#
#   promote.sh [--dry-run] [--force] <ref>
#
# EXPECTED MODEL: SPLIT-CHECKOUT
# --------------------------------
# SPIRA_PROD is a separate clone outside SPIRA_REPO. The development checkout (SPIRA_REPO)
# is where beads land and aeons work; this script carries landed commits across to the
# production checkout on a timer (spira-promote.timer, every 2 minutes). Set SPIRA_PROD
# in spira.conf to a path outside SPIRA_REPO (e.g. adjacent to the dev checkout, named
# with a "-prod" suffix). The
# first call creates the clone when absent; subsequent calls fast-forward it.
#
# In split-checkout mode a dirty or mid-landing SPIRA_REPO does not reach the executing
# copy. An aeon that leaves changes uncommitted cannot disrupt a live session.
#
# SINGLE-CHECKOUT LEGACY MODE (REFUSED)
# ---------------------------------------
# If SPIRA_PROD is inside SPIRA_REPO, promote.sh exits 1 and names skew.sh refresh as the
# correct tool. doctor.sh reports this as a FAIL. To fix: set SPIRA_PROD to a path outside
# SPIRA_REPO and run install.sh.
#
# UNIT RESTARTS (split-checkout). Only units whose ExecStart script changed between old
# and new are restarted. Others are left running, so a promotion of a change to one
# script does not interrupt every service.
#
# DRY RUN. --dry-run reports everything that would change without touching the
# production checkout or restarting any unit.
#
# EXIT   0  success
#        1  usage error, refused (non-fast-forward, unresolvable ref), or single-checkout
#        2  production checkout exists but is not a git repo
set -uo pipefail
. "$(dirname "$0")/lib.sh"

SC="${SPIRA_SYSTEMCTL:-systemctl}"
DRY_RUN=0; FORCE=0
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)    DRY_RUN=1; shift ;;
        --force)      FORCE=1;   shift ;;
        --) shift; break ;;
        -*) printf 'promote: unknown flag: %s\n' "$1" >&2; exit 1 ;;
        *)  break ;;
    esac
done
REF="${1:?usage: promote.sh [--dry-run] [--force] <ref>}"

[ -n "${SPIRA_PROD:-}" ] || die "promote: SPIRA_PROD is not set — set it in spira.conf"

# FETCH BEFORE RESOLVING. The timer typically passes 'origin/main' as the ref; the local
# tracking branch (plain 'main') is almost always stale because the landing pass pushes to
# the remote without fast-forwarding the local. A stale local 'main' resolves to a commit
# already behind the production checkout, triggering a false "not a fast-forward" error on
# every timer tick. Fetch unconditionally here so callers can name remote-tracking refs
# ('origin/main') and be certain they are current; plain branch names work too after the
# fetch updates their tracking refs.
git -C "$SPIRA_REPO" fetch origin 2>/dev/null || true

# SINGLE-CHECKOUT DETECTION. Resolve both paths to their canonical forms so a symlink
# or a trailing slash does not defeat the membership test.
_promote_prod="$(cd "$SPIRA_PROD" 2>/dev/null && pwd -P || printf '%s' "$SPIRA_PROD")"
_promote_repo="$(cd "$SPIRA_REPO" 2>/dev/null && pwd -P || printf '%s' "$SPIRA_REPO")"
case "$_promote_prod/" in
    "$_promote_repo/"*)
        printf 'promote: single-checkout mode: SPIRA_PROD (%s) is inside SPIRA_REPO (%s)\n' \
            "$SPIRA_PROD" "$SPIRA_REPO" >&2
        printf 'promote: in this model the landing pass is the only thing that advances the copy in force\n' >&2
        printf 'promote: to fast-forward the checkout manually: %s/skew.sh refresh\n' \
            "$(dirname "$0")" >&2
        exit 1
        ;;
esac
unset _promote_prod _promote_repo

# SPIRA_PROD is the harness subdir (e.g. spira/) inside the production checkout.
PROD_HOME="$SPIRA_PROD"
PROD_REPO="$(dirname "$PROD_HOME")"

# --- resolve the ref in the dev repo to a commit SHA ---
RESOLVED="$(git -C "$SPIRA_REPO" rev-parse --verify "${REF}^{commit}" 2>/dev/null)" || {
    printf 'promote: %s does not name a commit in the development repo (%s)\n' "$REF" "$SPIRA_REPO" >&2
    exit 1
}

# --- set up production checkout if it does not exist ---
if [ ! -d "$PROD_REPO/.git" ]; then
    log "promote: production checkout is absent — cloning from $SPIRA_REPO"
    if [ "$DRY_RUN" = 1 ]; then
        log "promote: DRY RUN: would clone $SPIRA_REPO -> $PROD_REPO"
    else
        mkdir -p "$(dirname "$PROD_REPO")"
        git clone --local "$SPIRA_REPO" "$PROD_REPO" 2>&1 \
            | while IFS= read -r line; do log "promote: clone: $line"; done
        clone_status=${PIPESTATUS[0]}
        [ "$clone_status" -eq 0 ] || { log "promote: clone failed (exit $clone_status)"; exit 1; }
        git -C "$PROD_REPO" checkout --detach "$RESOLVED" >/dev/null 2>&1
        log "promote: production checkout created at $PROD_REPO"
        log "promote: production is now at $RESOLVED"
        exit 0
    fi
fi

# The production checkout exists — validate and fast-forward.
[ -d "$PROD_REPO/.git" ] || { printf 'promote: %s exists but is not a git repo\n' "$PROD_REPO" >&2; exit 2; }

# LIVE-AEON GUARD. A promotion rewrites aeon.sh and lib.sh in the production checkout;
# those are the files running aeons are executing (law-replace-running-scripts-atomically).
# Scoped to the named instance: a test-instance promote does not refuse because prod aeons
# are running. Set SPIRA_PROMOTE_FORCE=1 to override (e.g. after world.sh stop confirms
# all aeons are gone but the guard fires on a stale unit state).
if [ -z "${SPIRA_PROMOTE_FORCE:-}" ]; then
    _promote_live="$(spira_live_aeons)"
    if [ -n "$_promote_live" ]; then
        if [ "$DRY_RUN" = 1 ]; then
            log "promote: DRY RUN: live aeons for instance $SPIRA_INSTANCE — would refuse:"
            printf '%s\n' "$_promote_live" | sed 's/^/promote:   /' >&2
        else
            printf 'promote: refusing — live aeons for instance %s would be disrupted:\n' \
                "$SPIRA_INSTANCE" >&2
            printf '%s\n' "$_promote_live" | sed 's/^/    /' >&2
            printf 'promote: wait for them to finish, or set SPIRA_PROMOTE_FORCE=1 to override.\n' >&2
            exit 1
        fi
    fi
    unset _promote_live
fi

OLD_HEAD="$(git -C "$PROD_REPO" rev-parse HEAD 2>/dev/null || true)"

# THE FENCE. The production checkout refuses commits outright. promote.sh re-asserts it on
# every pass, so a clone, a re-clone or a hand-deleted hook is repaired within one interval
# rather than leaving a window nobody is watching. `--no-verify` still bypasses it — which is
# why the self-heal below exists as well: refuse first, and repair whatever got through.
prod_fence_hooks() {
    local gitdir hook
    gitdir="$(git -C "$PROD_REPO" rev-parse --git-dir 2>/dev/null)" || return 0
    case "$gitdir" in /*) ;; *) gitdir="$PROD_REPO/$gitdir" ;; esac
    mkdir -p "$gitdir/hooks" 2>/dev/null || return 0
    hook="$gitdir/hooks/pre-commit"
    if [ -f "$hook" ] && grep -q 'SPIRA PRODUCTION CHECKOUT' "$hook" 2>/dev/null; then
        [ -x "$hook" ] || chmod +x "$hook" 2>/dev/null
        return 0
    fi
    if ! : > "${hook}.tmp.$$" 2>/dev/null; then
        # An unwritable hooks directory is not this script's problem to solve, and it must
        # not spray a raw shell error into the journal on every pass.
        log "promote: WARN: cannot write the commit fence at $hook (hooks dir not writable)"
        return 0
    fi
    cat > "${hook}.tmp.$$" <<'HOOK'
#!/usr/bin/env bash
# SPIRA PRODUCTION CHECKOUT — installed by promote.sh. Do not edit; edit promote.sh.
#
# This tree is a deploy artifact. It is what systemd ExecStarts, and promote.sh replaces it
# wholesale from the landref every two minutes. A commit here cannot reach the repository and
# cannot survive the next promotion, but it CAN diverge this checkout from the landref, which
# makes every subsequent promotion fail. That happened twice on 2026-09-11 and froze nineteen
# landings for hours.
#
# Commit in your own worktree instead. If you are an aeon, that is $SPIRA_WORK.
cat >&2 <<'MSG'
REFUSED: this is the Spira production checkout, not a working tree.

  Nothing may commit here. This tree is replaced from the landref on a timer, so a commit
  is lost either way -- but it first diverges the checkout and freezes every promotion
  after it.

  Commit in your own worktree. An aeon's is $SPIRA_WORK.
  Deliberate override (you will lose it at the next promote): git commit --no-verify
MSG
exit 1
HOOK
    chmod +x "${hook}.tmp.$$" 2>/dev/null
    mv "${hook}.tmp.$$" "$hook" 2>/dev/null \
        && log "promote: installed the production commit fence at $hook"
}
[ "$DRY_RUN" = 0 ] && prod_fence_hooks

prod_salvage_and_reset() {   # prod_salvage_and_reset <reason>
    local reason="$1" stamp patch
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    patch="${SPIRA_RUN:-/tmp}/reaped/prod-drift-${stamp}.patch"
    mkdir -p "$(dirname "$patch")" 2>/dev/null
    if git -C "$PROD_REPO" diff HEAD > "$patch" 2>/dev/null && [ -s "$patch" ]; then
        log "promote: DRIFT ($reason) — salvaged the production diff to $patch"
    else
        rm -f "$patch" 2>/dev/null
        log "promote: DRIFT ($reason) — nothing to salvage"
    fi
    git -C "$PROD_REPO" reset --hard >/dev/null 2>&1
    git -C "$PROD_REPO" clean -fd >/dev/null 2>&1
}

# Is the production tree dirty, or carrying commits the landref does not have?
PROD_DIRTY=""
[ -n "$(git -C "$PROD_REPO" status --porcelain --untracked-files=no 2>/dev/null)" ] \
    && PROD_DIRTY="a tracked file was modified in the production checkout"
if [ -z "$PROD_DIRTY" ] && [ -n "$OLD_HEAD" ] \
   && ! git -C "$SPIRA_REPO" cat-file -e "${OLD_HEAD}^{commit}" 2>/dev/null; then
    PROD_DIRTY="production HEAD ($OLD_HEAD) is a commit the development checkout has never seen"
fi

if [ -n "$PROD_DIRTY" ]; then
    if [ "$DRY_RUN" = 1 ]; then
        log "promote: DRY RUN: would reset the production checkout — $PROD_DIRTY"
    else
        prod_salvage_and_reset "$PROD_DIRTY"
        OLD_HEAD="$(git -C "$PROD_REPO" rev-parse HEAD 2>/dev/null)"
    fi
fi

# THE DRIFT CHECK RUNS BEFORE THE "NOTHING TO DO" EXIT, NOT AFTER IT.
# Drift does not have to move HEAD. On 2026-09-11 production sat at the right commit with
# spira/conf.sh modified in the tree, and while the landref was static every pass took the
# early exit below and healed nothing. A tree that is dirty is never "nothing to do".
if [ "$OLD_HEAD" = "$RESOLVED" ]; then
    log "promote: production is already at $RESOLVED — nothing to do"
    exit 0
fi

# FAST-FORWARD CHECK. The new ref must descend from the current production HEAD.
# --force skips this to allow rollback: promote to an earlier ref (reverting a bad
# promotion), then forward again once the fix is ready. Reversal is safe because
# promote.sh prints the current HEAD before overwriting it, so the rollback SHA is
# always in the log. Only units whose ExecStart changed are restarted in either direction.
if [ -n "$OLD_HEAD" ] && [ "$FORCE" = 0 ]; then
    if ! git -C "$SPIRA_REPO" merge-base --is-ancestor "$OLD_HEAD" "$RESOLVED" 2>/dev/null; then
        printf 'promote: %s is not a fast-forward from the current production HEAD (%s)\n' \
            "$RESOLVED" "$OLD_HEAD" >&2
        printf 'promote: to reverse: promote.sh --force %s\n' "$OLD_HEAD" >&2
        exit 1
    fi
fi

# REVIEWER GATE. A release tag with a stored BLOCK verdict may not be promoted
# to production. The reviewer files its findings as beads — resolve them, run
# a fresh review (review.sh <tag>), and promote again once the verdict is ship.
# A unit with no stored verdict passes silently: review.sh may not be configured
# yet, or this may be a pre-review rollback to a previously-shipped tag.
#
# ONLY the release-tag path is gated. Arbitrary ref promotions (e.g. a direct
# SHA or a branch tip without a release tag) pass through. That path exists for
# the initial clone and for emergency rollbacks, which must not be blocked by a
# gate that only knows about annotated release tags.
_REVIEW_VDIR="${SPIRA_REVIEWER_VERDICTS:-$SPIRA_RUN/review-verdicts}"
_REVIEW_TAG=""
# First: if the ref is itself a release tag name, use it directly.
case "$REF" in spira-release-*)
    git -C "$SPIRA_REPO" rev-parse --verify "refs/tags/$REF" >/dev/null 2>&1 \
        && _REVIEW_TAG="$REF" ;;
esac
# Second: look for release tags that point to the resolved commit.
if [ -z "$_REVIEW_TAG" ]; then
    _REVIEW_TAG="$(git -C "$SPIRA_REPO" tag -l 'spira-release-*' \
        --points-at "$RESOLVED" 2>/dev/null | sort | tail -1)"
fi
if [ -n "${_REVIEW_TAG:-}" ] && [ -f "$_REVIEW_VDIR/$_REVIEW_TAG.verdict" ]; then
    _REVIEW_STORED="$(grep '^verdict: ' "$_REVIEW_VDIR/$_REVIEW_TAG.verdict" \
        | sed 's/^verdict: //')"
    if [ "${_REVIEW_STORED:-}" = "block" ]; then
        if [ "$DRY_RUN" = 1 ]; then
            log "promote: DRY RUN: $_REVIEW_TAG has a BLOCK verdict — would refuse"
        else
            printf 'promote: release unit %s has a BLOCK verdict\n' "$_REVIEW_TAG" >&2
            printf 'promote: resolve the findings (label: %s) and re-run: review.sh %s\n' \
                "$SPIRA_REVIEW_LABEL" "$_REVIEW_TAG" >&2
            exit 1
        fi
    fi
fi

# --- find what changed in the harness subdir ---
# Only scripts inside the harness directory (e.g. "spira/") affect which units to restart.
HOME_SUB="$(basename "$PROD_HOME")"   # e.g. "spira"
declare -a changed_scripts=()
if [ -n "$OLD_HEAD" ]; then
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        changed_scripts+=("${f#${HOME_SUB}/}")
    done < <(git -C "$SPIRA_REPO" diff --name-only "$OLD_HEAD" "$RESOLVED" 2>/dev/null \
        | grep "^${HOME_SUB}/")
fi

# --- the production checkout is a DEPLOY ARTIFACT, never a working tree ---
#
# Nothing but this script may write $PROD_REPO. Twice on 2026-09-11 an aeon did: a commit
# straight onto it (10f4a54) diverged it from the landref and froze nineteen promotions for
# hours, and a second occurrence left it detached with spira/conf.sh modified. Neither was
# reported, because the advance below used to discard the exit code of `git checkout` and
# then log "done" unconditionally — so a promotion that moved nothing announced success.
#
# Three rules now hold, in this order, because each is the one the next depends on:
#   1. DRIFT SELF-HEALS. A dirty tree is salvaged to $SPIRA_RUN/reaped and reset. Drift can
#      exist for at most one promote interval, never for hours.
#   2. THE WRITE IS CHECKED. fetch and checkout exit codes decide whether we continue.
#   3. THE RESULT IS VERIFIED FROM THE TREE ITSELF, not from the fact that a command ran.
#      HEAD must equal RESOLVED and the tree must be clean before anything logs "done".
# law-landed-is-not-in-force.
# --- advance production ---
log "promote: $OLD_HEAD -> $RESOLVED"
if [ "$DRY_RUN" = 0 ]; then
    if ! git -C "$PROD_REPO" fetch "$SPIRA_REPO" "$RESOLVED" >/dev/null 2>&1; then
        log "promote: REFUSED — could not fetch $RESOLVED into the production checkout"
        exit 1
    fi
    if ! git -C "$PROD_REPO" checkout --detach "$RESOLVED" >/dev/null 2>&1; then
        log "promote: REFUSED — git checkout --detach $RESOLVED failed in $PROD_REPO"
        log "promote: production is STILL at $(git -C "$PROD_REPO" rev-parse HEAD 2>/dev/null)"
        exit 1
    fi
    # VERIFY FROM THE TREE. A command that ran is not evidence the tree moved.
    _prod_head="$(git -C "$PROD_REPO" rev-parse HEAD 2>/dev/null)"
    if [ "$_prod_head" != "$RESOLVED" ]; then
        log "promote: REFUSED — asked for $RESOLVED but production HEAD reads $_prod_head"
        exit 1
    fi
    if [ -n "$(git -C "$PROD_REPO" status --porcelain --untracked-files=no 2>/dev/null)" ]; then
        log "promote: REFUSED — production tree is dirty immediately after checkout"
        exit 1
    fi
fi

if [ "${#changed_scripts[@]}" -gt 0 ]; then
    log "promote: scripts changed in ${HOME_SUB}/: ${changed_scripts[*]}"
else
    log "promote: no scripts changed in ${HOME_SUB}/"
fi

# --- find units that reference changed scripts and restart them ---
UNIT_DIR="$HOME/.config/systemd/user"
declare -a restart_units=()
declare -A seen_units=()

if [ -d "$UNIT_DIR" ] && [ "${#changed_scripts[@]}" -gt 0 ]; then
    for script in "${changed_scripts[@]}"; do
        while IFS= read -r unit_file; do
            [ -f "$unit_file" ] || continue
            unit="$(basename "$unit_file")"
            # Match ExecStart/ExecStartPre/ExecCondition lines that name this script.
            # The installed unit has the actual path, e.g. /path/to/spira/sentinel.sh.
            if grep -qE "^(ExecStart|ExecStartPre|ExecCondition)=.*/${script}( |$)" \
                    "$unit_file" 2>/dev/null; then
                if [ -z "${seen_units[$unit]:-}" ]; then
                    restart_units+=("$unit")
                    seen_units["$unit"]=1
                fi
            fi
        done < <(find "$UNIT_DIR" -maxdepth 1 -name '*.service' -type f 2>/dev/null)
    done
fi

if [ "${#restart_units[@]}" -eq 0 ]; then
    log "promote: no installed units reference changed scripts — no restarts needed"
else
    for u in "${restart_units[@]}"; do
        log "promote: restarting $u"
        if [ "$DRY_RUN" = 0 ]; then
            "$SC" --user restart "$u" 2>/dev/null \
                || log "promote: WARN: failed to restart $u (may not be installed)"
        fi
    done
fi

log "promote: done — production is at $RESOLVED"
if [ -n "$OLD_HEAD" ]; then
    log "promote: to reverse this promotion: promote.sh $OLD_HEAD"
fi
