#!/usr/bin/env bash
#
# stage.sh — stand up / tear down a fully isolated Spira for testing.
#
#   eval "$(stage.sh up)"          — create a stage; prints an env block to eval
#   eval "$(stage.sh up /tmp/dir)" — use a specific root (must not exist yet)
#   stage.sh down "$STAGE_ROOT"    — tear down completely
#
# Everything the stage needs lives under STAGE_ROOT: its own git repo and bare
# remote, its own beads database, its own SPIRA_RUN, a minimal chamber, and the
# synthetic dispatch scripts that replace systemd-run at the two harness seams.
# Nothing inside the stage touches the caller's SPIRA_DB, SPIRA_RUN, or any
# real git repository.
#
# ISOLATION IS ASSERTED AT BUILD TIME. stage.sh up refuses to print an env
# block where any of SPIRA_DB, SPIRA_RUN, or SPIRA_HOME resolves to a path
# outside STAGE_ROOT, so a misconfiguration fails closed rather than silently
# contaminating a real instance.
#
# THE TWO HARNESS SEAMS:
#
#   SPIRA_SUMMON — replaces systemd-run for sentinel's CHECK 7 (aeon summon).
#     The stage writes fake-summon.sh here; it ignores all systemd flags and
#     execs canary-worker.sh synchronously in the inherited stage env. The
#     model is the one part canary cannot test, and this is the boundary.
#
#   SPIRA_LAUNCH — replaces systemd-run for sentinel's CHECK 6 (landing
#     dispatch). The stage writes fake-launch.sh here; it records the dispatch
#     timestamp (keeping sentinel's staleness accounting correct) and exits 0.
#     canary.sh runs landing.sh directly when it wants a landing pass.
#
# covers: spira/stage.sh spira/canary.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

die() { printf 'stage: %s\n' "$*" >&2; exit 1; }

# ─── up ──────────────────────────────────────────────────────────────────────
up() {
    local root="${1:-}"
    if [ -z "$root" ]; then
        root="$(mktemp -d)"
    else
        [ -d "$root" ] && die "up: $root already exists — stage.sh down it first"
        mkdir -p "$root"
    fi
    root="$(cd "$root" && pwd -P)"

    # ---- bd-embedded --------------------------------------------------------
    # Everything under STAGE_ROOT so a single rm -rf tears down completely.
    local bd_emb; bd_emb="$(command -v bd-embedded 2>/dev/null)"
    [ -n "$bd_emb" ] || die "bd-embedded not found; install: npm install -g @beads/bd"
    # A private bin dir so 'bd' resolves to the embedded binary in this process
    # tree without disturbing the caller's PATH permanently.
    mkdir -p "$root/bin"
    ln -sf "$bd_emb" "$root/bin/bd"

    # ---- beads database inside STAGE_ROOT -----------------------------------
    local db="$root/db"
    mkdir -p "$db"
    # FAST PATH: when the test harness pre-built a shared fixture and exported
    # TESTDB_BASELINE, copy its .beads snapshot instead of running bd init.
    # bd init costs ~10s (schema DDL); a directory copy costs ~4ms.  The baseline
    # is an empty-database snapshot taken by testdb.sh immediately after init, so
    # every copy is equivalent to a fresh init — but a test session that runs
    # stage.sh up seven times pays the cost once (when the shared fixture was
    # built) rather than seven times.
    if [ "${TESTDB_SHARED:-0}" = 1 ] && [ -d "${TESTDB_BASELINE:-}/.beads" ]; then
        cp -rp "$TESTDB_BASELINE/.beads" "$db/.beads" || {
            printf 'stage: baseline copy failed\n' >&2
            rm -rf "$root"; exit 1
        }
    else
        local init_out init_rc
        init_out="$(cd "$db" && env -i PATH="$root/bin:$PATH" HOME="$HOME" TERM=dumb \
            BD_NON_INTERACTIVE=1 \
            bd-embedded init --non-interactive --prefix sp --skip-agents --skip-hooks \
            -q 2>&1)"
        init_rc=$?
        [ "$init_rc" -eq 0 ] || {
            printf 'stage: bd init failed (rc=%s):\n%s\n' "$init_rc" "$init_out" >&2
            rm -rf "$root"; exit 1
        }
    fi

    # ---- git: bare remote + working checkout --------------------------------
    local remote="$root/remote.git" repo="$root/repo"
    git init -q --bare -b main "$remote"
    git init -q -b main "$repo"
    GIT_AUTHOR_NAME=stage GIT_AUTHOR_EMAIL=stage@example.invalid \
    GIT_COMMITTER_NAME=stage GIT_COMMITTER_EMAIL=stage@example.invalid \
        git -C "$repo" commit -q --allow-empty -m "stage: initial"
    git -C "$repo" remote add origin "$remote"
    git -C "$repo" push -q origin main
    git -C "$repo" fetch -q origin

    # ---- SPIRA_RUN ----------------------------------------------------------
    mkdir -p "$root/run/worktree"

    # ---- harness directory: SPIRA_HOME = $root/spira ------------------------
    # Symlink every non-test harness script from the REAL harness so the stage
    # runs the actual code — not a copy frozen at setup time. The stage-specific
    # pieces (chamber, repo-map, fake scripts) are written directly.
    local sh="$root/spira"
    mkdir -p "$sh/chamber"
    for f in "$HERE"/*.sh "$HERE"/*.py; do
        [ -f "$f" ] || continue
        local bn; bn="$(basename "$f")"
        case "$bn" in
            test-*|stage.sh|canary.sh) continue ;;
        esac
        ln -sf "$f" "$sh/$bn"
    done

    # ---- chamber: one canary fayth ------------------------------------------
    # FAYTH_LABELS must include SPIRA_SCOPE_LABEL (when non-empty) to pass fayth_fenced.
    # FAYTH_MAX_CONCURRENT=1 lets sentinel summon exactly one worker.
    cat > "$sh/chamber/canary.fayth" <<'FAYTH'
FAYTH_NAME=canary
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan"
FAYTH_MAX_CONCURRENT=1
FAYTH

    # ---- repo-map -----------------------------------------------------------
    # Six-column format required by doctor.sh and landing.sh.
    # The repo name must match the basename of SPIRA_REPO so spira_home_repo()
    # resolves correctly when SPIRA_HOME_REPO is not explicitly set.
    # Empty gate column = syntax check only, which is right for a synthetic repo.
    local repo_name; repo_name="$(basename "$repo")"
    printf '%s | %s | push | origin/main | |\n' "$repo_name" "$repo" \
        > "$sh/repo-map"

    # ---- fake-summon.sh (SPIRA_SUMMON) --------------------------------------
    # Sentinel's CHECK 7 calls:
    #   ${SPIRA_SUMMON} --user --collect --quiet --unit=... --setenv=... aeon.sh <fayth>
    # We ignore all systemd flags and exec the canary worker instead. The
    # worker inherits the full stage env from sentinel's own environment, so
    # no --setenv parsing is needed.
    cat > "$sh/fake-summon.sh" <<'SCRIPT'
#!/usr/bin/env bash
# Replaces systemd-run for sentinel CHECK 7. Runs canary-worker synchronously.
exec "$(dirname "$0")/canary-worker.sh"
SCRIPT
    chmod +x "$sh/fake-summon.sh"

    # ---- fake-launch.sh (SPIRA_LAUNCH) --------------------------------------
    # Sentinel's CHECK 6 calls:
    #   ${SPIRA_LAUNCH} --user --collect --quiet --unit=... --setenv=... landing.sh
    # We record the dispatch timestamp (so sentinel's staleness checks work)
    # and exit 0. canary.sh runs landing.sh directly when it wants a landing pass.
    cat > "$sh/fake-launch.sh" <<'SCRIPT'
#!/usr/bin/env bash
# Replaces systemd-run for sentinel CHECK 6. Records dispatch, exits 0.
# canary.sh drives landing.sh directly.
[ -n "${SPIRA_RUN:-}" ] && date +%s > "$SPIRA_RUN/landing.dispatched"
exit 0
SCRIPT
    chmod +x "$sh/fake-launch.sh"

    # ---- canary-worker.sh ---------------------------------------------------
    # The synthetic aeon: claims a ready bead, commits to its branch, closes it.
    # Invoked synchronously by fake-summon.sh; inherits the full stage env.
    cat > "$sh/canary-worker.sh" <<'SCRIPT'
#!/usr/bin/env bash
# canary-worker.sh — synthetic aeon for the stage canary.
# Claims one ready bead, commits to its branch, closes it.
# All stage env vars (SPIRA_HOME, SPIRA_RUN, SPIRA_DB, SPIRA_REPO) inherited.
set -uo pipefail
# shellcheck disable=SC1090
. "$(dirname "$0")/lib.sh"

_cw_log() { printf '%s canary-worker: %s\n' "$(date -u +%H:%M:%SZ)" "$*" >&2; }

# Claim the first ready bead in the canary partition.
_cw_claimed="$(bdq ready --limit 0 --exclude-type epic,event -u \
    --claim --label "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" --json 2>/dev/null)"
_cw_id="$(printf '%s' "$_cw_claimed" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
d = d if isinstance(d, list) else [d]
if not d: sys.exit(1)
print(d[0]["id"])
' 2>/dev/null)" || { _cw_log "nothing ready to claim"; exit 0; }

[ -n "${_cw_id:-}" ] || { _cw_log "nothing ready to claim"; exit 0; }
_cw_log "claimed $_cw_id"

# Create the branch in the stage repo, commit, and push.
_cw_br="spira/$_cw_id"
_cw_repo="${SPIRA_REPO:-}"
[ -n "$_cw_repo" ] || { _cw_log "SPIRA_REPO not set"; bdq release "$_cw_id" >/dev/null 2>&1; exit 1; }

git -C "$_cw_repo" checkout -q -b "$_cw_br" origin/main 2>/dev/null \
    || git -C "$_cw_repo" checkout -q "$_cw_br" 2>/dev/null \
    || { _cw_log "could not create branch $_cw_br"; bdq release "$_cw_id" >/dev/null 2>&1; exit 1; }

printf '%s\n' "$_cw_id" > "$_cw_repo/canary.txt"
GIT_AUTHOR_NAME=canary GIT_AUTHOR_EMAIL=canary@example.invalid \
GIT_COMMITTER_NAME=canary GIT_COMMITTER_EMAIL=canary@example.invalid \
    git -C "$_cw_repo" add canary.txt
GIT_AUTHOR_NAME=canary GIT_AUTHOR_EMAIL=canary@example.invalid \
GIT_COMMITTER_NAME=canary GIT_COMMITTER_EMAIL=canary@example.invalid \
    git -C "$_cw_repo" commit -q -m "feat: $_cw_id — canary synthetic commit"
git -C "$_cw_repo" push -q origin "$_cw_br" 2>/dev/null \
    || { _cw_log "push failed for $_cw_br"; exit 1; }
_cw_log "committed and pushed $_cw_br"

# Record the branch affinity and close the bead.
bdq set-state "$_cw_id" "branch=$_cw_br" >/dev/null 2>&1 || true
bdq close "$_cw_id" --reason "canary-worker: committed on $_cw_br" >/dev/null 2>&1 \
    || { _cw_log "close failed for $_cw_id"; exit 1; }
_cw_log "closed $_cw_id"
SCRIPT
    chmod +x "$sh/canary-worker.sh"

    # ---- isolation check ----------------------------------------------------
    for _p in "$db" "$root/run" "$sh"; do
        case "$_p" in
            "$root"/*|"$root") ;;
            *) rm -rf "$root"; die "isolation violated: $_p is outside $root"; ;;
        esac
    done
    unset _p

    # ---- print eval-able env block ------------------------------------------
    printf 'export STAGE_ROOT=%s\n'      "$root"
    printf 'export SPIRA_HOME=%s\n'      "$sh"
    printf 'export SPIRA_RUN=%s\n'       "$root/run"
    printf 'export SPIRA_DB=%s\n'        "$db"
    printf 'export SPIRA_BD=bd-embedded\n'
    printf 'export SPIRA_REPO=%s\n'      "$repo"
    printf 'export SPIRA_REPO_MAP=%s\n'  "$sh/repo-map"
    printf 'export SPIRA_FAYTHS=canary\n'
    printf 'export SPIRA_MAX_AEONS=1\n'
    # Escalations: /bin/true accepts all args and exits 0.
    printf 'export SPIRA_NOTIFY=/bin/true\n'
    printf 'export SPIRA_SUMMON=%s\n'    "$sh/fake-summon.sh"
    printf 'export SPIRA_LAUNCH=%s\n'    "$sh/fake-launch.sh"
    # PATH: prepend the bd shim so 'bd' resolves to bd-embedded in child processes.
    printf 'export PATH=%s:${PATH}\n'    "$root/bin"
    printf 'export SPIRA_PATH=%s\n'      "$root/bin"
    # Stage operates without a scope label so sentinel, canary-worker, and bead creation
    # all see an empty value — the conf.sh default of 'spira' would make them query for
    # 'spira,plan' while the bead is created with 'plan' only, and plan_ready stays 0.
    printf 'export SPIRA_SCOPE_LABEL=\n'
}

# ─── down ────────────────────────────────────────────────────────────────────
down() {
    local root="${1:?usage: stage.sh down <root>}"
    root="$(cd "$root" 2>/dev/null && pwd -P)" \
        || die "down: $1 does not exist"
    # Safety check: only remove a path that looks like a stage root.
    [ -f "$root/spira/chamber/canary.fayth" ] \
        || die "down: $root does not look like a stage (no canary.fayth) — refusing rm -rf"
    rm -rf "$root"
}

# ─── dispatch ────────────────────────────────────────────────────────────────
case "${1:-up}" in
    up)   up "${2:-}" ;;
    down) down "${2:?usage: stage.sh down <STAGE_ROOT>}" ;;
    *)    sed -n '3,9p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
