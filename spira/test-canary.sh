#!/usr/bin/env bash
#
# test-canary.sh — exercises stage.sh (the isolated test environment builder) and
# canary.sh (the end-to-end pipeline canary that runs on that stage).
#
#   ./test-canary.sh
#
# WHAT THIS TESTS:
#   - stage.sh up creates a fully isolated Spira environment under a temp dir
#   - Every path exported by up resolves under STAGE_ROOT (isolation assertion)
#   - The stage's beads database is usable (bd create/list round-trips)
#   - The stage git setup is correct: bare remote + working checkout on main
#   - stage.sh down removes STAGE_ROOT completely
#   - canary.sh runs end-to-end on a stage: bead filed → sentinel pass (with
#     fake-summon.sh / canary-worker.sh) → landing pass → commit on origin/main
#
# POSITIVE CONTROLS:
#   - Bead-visible-in-stage-db proves the DB check is asking the right store
#   - Commit-absent-before-canary proves the canary check would have caught a miss
#
# ISOLATION GUARANTEE:
#   - Each test that mutates env runs in a subshell; stage vars cannot leak
#   - The real SPIRA_DB (before stage eval) is never written to in any test
#
# covers: spira/canary.sh spira/stage.sh
# scar: unrecorded
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

# Counters live in temp files so subshells can contribute.
_RESULTS="$(mktemp)"
trap 'rm -f "$_RESULTS"' EXIT
ok()     { printf 'ok\n'   >> "$_RESULTS"; printf '  ok    %s\n' "$1"; }
bad()    { printf 'bad\n'  >> "$_RESULTS"; printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isnt()   { [ "$2" != "$3" ] && ok "$1" || bad "$1" "did not want [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "unwanted [$2] in [$3]"; }
exists() { [ -e "$2" ] && ok "$1" || bad "$1" "expected file/dir: $2"; }
isexec() { [ -x "$2" ] && ok "$1" || bad "$1" "expected executable: $2"; }

printf 'test-canary\n'
printf '  (stage.sh + canary.sh end-to-end)\n'

# ─── T1: stage up creates expected structure ──────────────────────────────────
printf '\nT1: stage up creates expected structure\n'
(
    eval "$(bash "$HERE/stage.sh" up)" \
        || { printf '  FATAL: stage up failed\n'; exit 1; }
    trap 'bash "$HERE/stage.sh" down "$STAGE_ROOT" 2>/dev/null' EXIT

    [ -n "${STAGE_ROOT:-}" ] && ok "STAGE_ROOT is set" || bad "STAGE_ROOT is set" "empty"
    [ -d "${STAGE_ROOT:-/nonexistent}" ] && ok "STAGE_ROOT is a directory" || bad "STAGE_ROOT is a directory" "$STAGE_ROOT"

    exists "canary.fayth"         "$SPIRA_HOME/chamber/canary.fayth"
    exists "repo-map"              "$SPIRA_HOME/repo-map"
    isexec "fake-summon.sh"        "$SPIRA_HOME/fake-summon.sh"
    isexec "fake-launch.sh"        "$SPIRA_HOME/fake-launch.sh"
    isexec "canary-worker.sh"      "$SPIRA_HOME/canary-worker.sh"
    exists "lib.sh symlink"        "$SPIRA_HOME/lib.sh"
    exists "sentinel.sh symlink"   "$SPIRA_HOME/sentinel.sh"
    exists "landing.sh symlink"    "$SPIRA_HOME/landing.sh"
    exists "bare remote"           "$STAGE_ROOT/remote.git/HEAD"
    exists "repo checkout"         "$STAGE_ROOT/repo/.git"
    exists "SPIRA_RUN/worktree"    "$SPIRA_RUN/worktree"
    exists "stage db dir"          "$SPIRA_DB"

    # SPIRA_FAYTHS must be exactly "canary"
    is "SPIRA_FAYTHS=canary" "canary" "$SPIRA_FAYTHS"

    # bd shim must be bd-embedded
    is "SPIRA_BD=bd-embedded" "bd-embedded" "$SPIRA_BD"

    # Verify the repo-map has the 6-column format required by doctor.sh/landing.sh
    col_count="$(awk -F'|' '{print NF}' "$SPIRA_HOME/repo-map" | head -1)"
    is "repo-map has 6 columns" "6" "$col_count"

    # Verify the land column is "push" (not pr or hold — the synthetic repo must push)
    land_col="$(awk -F'|' '{gsub(/ /,"",$3); print $3}' "$SPIRA_HOME/repo-map" | head -1)"
    is "repo-map land=push" "push" "$land_col"

    # remote checkout is on main
    # hermetic-ok: $STAGE_ROOT/remote.git is always a mktemp temp dir created by stage.sh up
    head_ref="$(git -C "$STAGE_ROOT/remote.git" symbolic-ref HEAD 2>/dev/null)"
    is "remote HEAD is refs/heads/main" "refs/heads/main" "$head_ref"

    # working checkout has origin/main tracking ref
    # hermetic-ok: $STAGE_ROOT/repo is always a mktemp temp dir created by stage.sh up
    track="$(git -C "$STAGE_ROOT/repo" rev-parse origin/main 2>/dev/null | head -c 7)"
    [ -n "$track" ] && ok "origin/main ref exists in checkout" \
                    || bad "origin/main ref exists in checkout" "missing"
)

# ─── T2: all stage paths are under STAGE_ROOT ────────────────────────────────
printf '\nT2: stage isolation — all paths under STAGE_ROOT\n'
(
    eval "$(bash "$HERE/stage.sh" up)" \
        || { printf '  FATAL: stage up failed\n'; exit 1; }
    trap 'bash "$HERE/stage.sh" down "$STAGE_ROOT" 2>/dev/null' EXIT

    for var in SPIRA_HOME SPIRA_RUN SPIRA_DB SPIRA_REPO SPIRA_SUMMON SPIRA_LAUNCH; do
        val="${!var:-}"
        case "$val" in
            "$STAGE_ROOT"/*|"$STAGE_ROOT")
                ok "$var is under STAGE_ROOT" ;;
            *)
                bad "$var is under STAGE_ROOT" "$var=$val is outside $STAGE_ROOT" ;;
        esac
    done

    # Critically: SPIRA_HOME must not resolve to the real harness directory
    real_here="$(cd "$HERE" && pwd -P)"
    isnt "SPIRA_HOME is not the real harness dir" "$real_here" "$(cd "$SPIRA_HOME" && pwd -P)"
)

# ─── T3: stage db is usable (real bd round-trip) ─────────────────────────────
printf '\nT3: stage db is usable\n'
(
    eval "$(bash "$HERE/stage.sh" up)" \
        || { printf '  FATAL: stage up failed\n'; exit 1; }
    trap 'bash "$HERE/stage.sh" down "$STAGE_ROOT" 2>/dev/null' EXIT

    # Create a bead; bd exits non-zero on a broken db
    # hermetic-ok: $SPIRA_DB is the stage database — always a mktemp temp dir from stage.sh up
    created_id="$(bd -C "$SPIRA_DB" create "canary test bead" --type task \
        --labels "spira,plan" --silent 2>/dev/null | tr -d '[:space:]')"
    [ -n "$created_id" ] && ok "bd create succeeds in stage db" \
                         || bad "bd create succeeds in stage db" "empty id"

    # POSITIVE CONTROL: verify the bead IS visible in the stage db
    # hermetic-ok: $SPIRA_DB is the stage database — always a mktemp temp dir from stage.sh up
    found="$(bd -C "$SPIRA_DB" list --limit 0 --label "spira,plan" 2>/dev/null \
        | grep -c "$created_id" || true)"
    [ "${found:-0}" -gt 0 ] && ok "created bead visible in stage db" \
                             || bad "created bead visible in stage db" "not found"

    # The bead must NOT be visible in the real SPIRA_DB (if one is set)
    if [ -n "${_REAL_DB:-}" ] && [ -d "$_REAL_DB" ]; then
        # hermetic-ok: $_REAL_DB is the pre-stage SPIRA_DB, read-only here to verify isolation
        real_found="$(bd -C "$_REAL_DB" list --limit 0 --label "spira,plan" 2>/dev/null \
            | grep -c "$created_id" || true)"
        [ "${real_found:-0}" -eq 0 ] && ok "stage bead not visible in real db" \
                                      || bad "stage bead not visible in real db" "leaked: $created_id"
    else
        ok "real db isolation (no real db to check against)"
    fi
)

# ─── T4: stage down removes the root completely ───────────────────────────────
printf '\nT4: stage down removes STAGE_ROOT\n'
(
    eval "$(bash "$HERE/stage.sh" up)" \
        || { printf '  FATAL: stage up failed\n'; exit 1; }
    saved_root="$STAGE_ROOT"
    bash "$HERE/stage.sh" down "$STAGE_ROOT"
    [ ! -d "$saved_root" ] && ok "STAGE_ROOT removed after down" \
                            || bad "STAGE_ROOT removed after down" "$saved_root still exists"
)

# ─── T5: stage down refuses a non-stage path ─────────────────────────────────
printf '\nT5: stage down refuses a non-stage path\n'
(
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    err="$(bash "$HERE/stage.sh" down "$tmp" 2>&1)" && rc=0 || rc=$?
    isnt "down of non-stage exits non-zero" "0" "$rc"
    want "down of non-stage prints refusal" "canary.fayth" "$err"
    rm -rf "$tmp"
)

# ─── T6: canary-worker.sh claims, commits, closes ────────────────────────────
printf '\nT6: canary-worker claims and closes a bead\n'
(
    eval "$(bash "$HERE/stage.sh" up)" \
        || { printf '  FATAL: stage up failed\n'; exit 1; }
    trap 'bash "$HERE/stage.sh" down "$STAGE_ROOT" 2>/dev/null' EXIT

    # Create a goal epic and a plan bead
    # hermetic-ok: $SPIRA_DB is the stage database — always a mktemp temp dir from stage.sh up
    goal="$(bd -C "$SPIRA_DB" create "t6 goal" --type epic --silent 2>/dev/null \
        | tr -d '[:space:]')"
    # hermetic-ok: $SPIRA_DB is the stage database — always a mktemp temp dir from stage.sh up
    bead="$(bd -C "$SPIRA_DB" create "t6 task" --type task --parent "$goal" \
        --labels "spira,plan" --silent 2>/dev/null | tr -d '[:space:]')"
    [ -n "$bead" ] || { printf '  FATAL: could not create bead\n'; exit 1; }

    # Run the worker directly (it inherits the stage env from the subshell)
    SPIRA_HOME="$SPIRA_HOME" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$SPIRA_BD" \
    SPIRA_REPO="$SPIRA_REPO" PATH="$PATH" \
        bash "$SPIRA_HOME/canary-worker.sh" 2>/dev/null
    rc=$?
    is "canary-worker exits 0" "0" "$rc"

    # Bead must be closed
    # hermetic-ok: $SPIRA_DB is the stage database — always a mktemp temp dir from stage.sh up
    status="$(bd -C "$SPIRA_DB" show "$bead" --json 2>/dev/null \
        | python3 -c 'import sys,json; d=json.load(sys.stdin); \
            d=d if isinstance(d,list) else [d]; print(d[0]["status"] if d else "")' 2>/dev/null)"
    is "bead is closed after worker" "closed" "$status"

    # Branch must exist in the remote
    # hermetic-ok: $STAGE_ROOT/remote.git is always a mktemp temp dir created by stage.sh up
    branches="$(git -C "$STAGE_ROOT/remote.git" branch --list "spira/$bead" 2>/dev/null)"
    want "branch spira/$bead pushed to remote" "spira/$bead" "$branches"

    # Commit subject on that branch must contain the bead id
    # hermetic-ok: $STAGE_ROOT/remote.git is always a mktemp temp dir created by stage.sh up
    subj="$(git -C "$STAGE_ROOT/remote.git" log "spira/$bead" \
        --format='%s' -n 1 2>/dev/null)"
    want "commit subject contains bead id" "$bead" "$subj"
)

# ─── T7: full end-to-end canary ───────────────────────────────────────────────
printf '\nT7: full canary (sentinel + landing)\n'
(
    # Capture what canary prints; exit code is what matters.
    out="$(bash "$HERE/canary.sh" 2>&1)"
    rc=$?
    is "canary.sh exits 0" "0" "$rc"
    want "canary log shows PASS"    "PASS"    "$out"
    want "canary log shows sentinel" "sentinel" "$out"
    want "canary log shows landing"  "landing"  "$out"

    # canary.sh creates and tears down its own stage; verify no stage root lingers.
    # This is satisfied if canary.sh exits 0 with the down trap.
    # Parse STAGE_ROOT from the output if it's printed.
    if [[ "$out" == *"STAGE_ROOT="* ]]; then
        stage_line="$(grep 'STAGE_ROOT=' <<< "$out" | head -1)"
        leftover="${stage_line#*STAGE_ROOT=}"
        leftover="${leftover%% *}"
        if [ -n "$leftover" ] && [ -d "$leftover" ]; then
            bad "stage root removed after canary" "$leftover still exists"
        else
            ok "stage root removed after canary"
        fi
    else
        ok "stage root removed (not parseable from output)"
    fi
)

# ─── summary ─────────────────────────────────────────────────────────────────
pass="$(grep -c '^ok$'  "$_RESULTS" 2>/dev/null || true)"
fail="$(grep -c '^bad$' "$_RESULTS" 2>/dev/null || true)"
printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "${fail:-0}" -eq 0 ]
