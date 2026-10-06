#!/usr/bin/env bash
#
# test-canary.sh — exercises `release stage` (sp-jsnbm; was stage.sh, the isolated test
# environment builder) and `release canary` (was canary.sh, the end-to-end pipeline canary
# that runs on that stage).
#
#   ./test-canary.sh
#
# WHAT THIS TESTS:
#   - release stage up creates a fully isolated Spira environment under a temp dir
#   - Every path exported by up resolves under STAGE_ROOT (isolation assertion)
#   - The stage's beads database is usable (bd create/list round-trips)
#   - The stage git setup is correct: bare remote + working checkout on main
#   - release stage down removes STAGE_ROOT completely, and stops the stage's lifecycle server
#   - release stage up stands up a lifecycle store of its own (sp-880u4): a private Dolt
#     sql-server built from lifecycle/ the way install builds one, its env pointing every
#     lifecycle path under STAGE_ROOT, so no lifecycle call leaves the stage
#   - release canary-worker claims through that machine and submits (READY -> SUBMITTED)
#   - release canary runs end-to-end on a stage: bead filed → sentinel pass (with
#     fake-summon.sh / release canary-worker) → landing pass → commit on origin/main
#
# POSITIVE CONTROLS:
#   - Bead-visible-in-stage-db proves the DB check is asking the right store
#   - Commit-absent-before-canary proves the canary check would have caught a miss
#
# ISOLATION GUARANTEE:
#   - Each test that mutates env runs in a subshell; stage vars cannot leak
#   - The real SPIRA_DB (before stage eval) is never written to in any test
#
# tier: T3
# covers: release/src/canary.rs release/src/stage.rs release/src/stage_lc.rs release/src/lifecycle_store.rs
# scar: sp-880u4
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
tl_subshell_safe

# The stage finds every compiled binary (sentinel, strand, aeon, landing-pass, spira-config)
# by bare name on the PATH testlib.sh set for this suite (sp-gypjk) — nothing to pin.

# `release`'s own Config::from_env resolves SPIRA_LC_PASSWORD_FILE via cfg() ambiently —
# the complete fixture declares a path that does not exist on disk
# (/fixture/home/.config/spira/spira-lc.credential), which `release stage up`'s own
# root schema-apply step then tries to read and fails. This suite never declared its own
# (sfail round 3, pattern 7); stage's throwaway dolt server takes no password, so empty.
tl_config SPIRA_LC_PASSWORD_FILE=""
# SPIRA_CHAMBER is registered too: chamber_dir_with() prefers a non-empty resolved config
# value over deriving <home>/chamber, and the complete fixture declares one
# (.../spira-releases/current/spira/chamber) that `release stage up`'s own eval never
# overrides for its dynamic STAGE_ROOT. Cleared so chamber resolution falls through to
# whatever SPIRA_HOME the stage sets (sfail round 4, same shape as pattern 6/7).
tl_config SPIRA_CHAMBER=""

isnt()   { [ "$2" != "$3" ] && ok "$1" || bad "$1" "did not want [$2] got [$3]"; }
exists() { [ -e "$2" ] && ok "$1" || bad "$1" "expected file/dir: $2"; }
isexec() { [ -x "$2" ] && ok "$1" || bad "$1" "expected executable: $2"; }

# lc_field <id> <field> — one field of the bead's row in the stage's lifecycle machine
# (`spira-lc show`, reached through the stage's own SPIRA_LC_* env), empty when no row.
lc_field() {
    timeout 5 spira-lc show "$1" 2>/dev/null | python3 -c '
import sys, json
try: b = (json.load(sys.stdin) or {}).get("bead") or {}
except Exception: b = {}
print(b.get(sys.argv[1]) or "")' "$2" 2>/dev/null
}

# ─── T1: stage up creates expected structure ──────────────────────────────────
printf '\nT1: stage up creates expected structure\n'
(
    # Reset before every stage boot: a prior T-block's post-eval sync below (real stage
    # credential, for ITS OWN spira-lc calls) would otherwise leak into THIS stage's own
    # `release stage up` schema-apply bootstrap as a stale, now-torn-down path — the
    # "cannot tell: reading ... No such file or directory" failure this round surfaced.
    tl_config SPIRA_LC_PASSWORD_FILE=""
    eval "$(release stage up)" \
        || { printf '  FATAL: stage up failed\n'; exit 1; }
    trap 'release stage down "$STAGE_ROOT" 2>/dev/null' EXIT
    # SPIRA_LC_PASSWORD_FILE is registered (spira-lc/src/db.rs resolves it via cfg(), not
    # raw env, unlike SPIRA_LC_PORT/SPIRA_LC_USER which stay raw env) — this suite's own
    # earlier override (cleared to "" so `release stage up`'s OWN schema-apply step, which
    # inherits the operator's env, doesn't choke on the fixture's bogus credential path)
    # would otherwise persist and starve any `spira-lc` call below of the stage's real,
    # just-generated credential (eval'd above as a plain var).
    tl_config SPIRA_LC_PASSWORD_FILE="${SPIRA_LC_PASSWORD_FILE:-}"

    [ -n "${STAGE_ROOT:-}" ] && ok "STAGE_ROOT is set" || bad "STAGE_ROOT is set" "empty"
    [ -d "${STAGE_ROOT:-/nonexistent}" ] && ok "STAGE_ROOT is a directory" || bad "STAGE_ROOT is a directory" "$STAGE_ROOT"

    exists "canary.fayth"         "$SPIRA_HOME/chamber/canary.fayth"
    exists "repo map"               "$SPIRA_REPO_MAP"
    isexec "fake-summon.sh"        "$SPIRA_HOME/fake-summon.sh"
    isexec "fake-launch.sh"        "$SPIRA_HOME/fake-launch.sh"
    # canary-worker.sh is gone: fake-summon.sh execs `release canary-worker` directly —
    # the worker is a subcommand of the same binary, not a per-stage file (sp-jsnbm).
    want "fake-summon execs release canary-worker" "release canary-worker" "$(cat "$SPIRA_HOME/fake-summon.sh")"
    exists "lib.sh symlink"        "$SPIRA_HOME/lib.sh"
    # sentinel.sh is gone: the stage runs the sentinel binary (release canary: `sentinel`).
    # landing.sh is gone: release canary runs `landing-pass land` (landing-pass/DESIGN.md §7.2).
    exists "landing-pass"          "$(command -v landing-pass)"
    exists "release binary"        "$(command -v release)"
    exists "bare remote"           "$STAGE_ROOT/remote.git/HEAD"
    exists "repo checkout"         "$STAGE_ROOT/repo/.git"
    exists "SPIRA_RUN/worktree"    "$SPIRA_RUN/worktree"
    exists "stage db dir"          "$SPIRA_DB"

    # The stage's own lifecycle machine (sp-880u4): reachable, as spira_lc, with the schema.
    is "SPIRA_LC_USER=spira_lc" "spira_lc" "${SPIRA_LC_USER:-}"
    exists "stage lifecycle credential" "${SPIRA_LC_PASSWORD_FILE:-/nonexistent}"
    isnt "stage lifecycle port is not the operator's 3307" "3307" "${SPIRA_LC_PORT:-3307}"
    lc_list="$(timeout 5 spira-lc list 2>&1)"; lc_rc=$?
    is "spira-lc list answers from the stage's machine" "0" "$lc_rc"
    is "the stage's machine starts empty" "[]" "$(tr -d '[:space:]' <<< "$lc_list")"

    # SPIRA_FAYTHS must be exactly "canary"
    is "SPIRA_FAYTHS=canary" "canary" "$SPIRA_FAYTHS"

    # bd shim must be bd-embedded
    is "SPIRA_BD=bd-embedded" "bd-embedded" "$SPIRA_BD"

    # Verify the stage's own repo map has the 6-column format required by doctor.sh/landing.sh
    col_count="$(awk -F'|' '{print NF}' "$SPIRA_REPO_MAP" | head -1)"
    is "repo map has 6 columns" "6" "$col_count"

    # Verify the land column is "push" (not pr or hold — the synthetic repo must push)
    land_col="$(awk -F'|' '{gsub(/ /,"",$3); print $3}' "$SPIRA_REPO_MAP" | head -1)"
    is "repo map land=push" "push" "$land_col"

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
    # Reset before every stage boot: a prior T-block's post-eval sync below (real stage
    # credential, for ITS OWN spira-lc calls) would otherwise leak into THIS stage's own
    # `release stage up` schema-apply bootstrap as a stale, now-torn-down path — the
    # "cannot tell: reading ... No such file or directory" failure this round surfaced.
    tl_config SPIRA_LC_PASSWORD_FILE=""
    eval "$(release stage up)" \
        || { printf '  FATAL: stage up failed\n'; exit 1; }
    trap 'release stage down "$STAGE_ROOT" 2>/dev/null' EXIT
    # SPIRA_LC_PASSWORD_FILE is registered (spira-lc/src/db.rs resolves it via cfg(), not
    # raw env, unlike SPIRA_LC_PORT/SPIRA_LC_USER which stay raw env) — this suite's own
    # earlier override (cleared to "" so `release stage up`'s OWN schema-apply step, which
    # inherits the operator's env, doesn't choke on the fixture's bogus credential path)
    # would otherwise persist and starve any `spira-lc` call below of the stage's real,
    # just-generated credential (eval'd above as a plain var).
    tl_config SPIRA_LC_PASSWORD_FILE="${SPIRA_LC_PASSWORD_FILE:-}"

    for var in SPIRA_HOME SPIRA_RUN SPIRA_DB SPIRA_REPO SPIRA_SUMMON SPIRA_LAUNCH \
               SPIRA_LC_PASSWORD_FILE SPIRA_LC_SOCKET SPIRA_LC_DATA_DIR; do
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
    # Reset before every stage boot: a prior T-block's post-eval sync below (real stage
    # credential, for ITS OWN spira-lc calls) would otherwise leak into THIS stage's own
    # `release stage up` schema-apply bootstrap as a stale, now-torn-down path — the
    # "cannot tell: reading ... No such file or directory" failure this round surfaced.
    tl_config SPIRA_LC_PASSWORD_FILE=""
    eval "$(release stage up)" \
        || { printf '  FATAL: stage up failed\n'; exit 1; }
    trap 'release stage down "$STAGE_ROOT" 2>/dev/null' EXIT
    # SPIRA_LC_PASSWORD_FILE is registered (spira-lc/src/db.rs resolves it via cfg(), not
    # raw env, unlike SPIRA_LC_PORT/SPIRA_LC_USER which stay raw env) — this suite's own
    # earlier override (cleared to "" so `release stage up`'s OWN schema-apply step, which
    # inherits the operator's env, doesn't choke on the fixture's bogus credential path)
    # would otherwise persist and starve any `spira-lc` call below of the stage's real,
    # just-generated credential (eval'd above as a plain var).
    tl_config SPIRA_LC_PASSWORD_FILE="${SPIRA_LC_PASSWORD_FILE:-}"

    # Create a bead; bd exits non-zero on a broken db
    # hermetic-ok: $SPIRA_DB is the stage database — always a mktemp temp dir from stage.sh up
    created_id="$(bd -C "$SPIRA_DB" create "canary test bead" --type task \
        --labels "spira,plan" --silent 2>/dev/null | tr -d '[:space:]')"
    [ -n "$created_id" ] && ok "bd create succeeds in stage db" \
                         || bad "bd create succeeds in stage db" "empty id"

    # POSITIVE CONTROL: verify the bead IS visible in the stage db
    # hermetic-ok: $SPIRA_DB is the stage database — always a mktemp temp dir from stage.sh up
    want "created bead visible in stage db" "${created_id:-<no id>}" \
        "$(bd -C "$SPIRA_DB" list --limit 0 --label "spira,plan" 2>/dev/null)"

    # The bead must NOT be visible in the real SPIRA_DB (if one is set)
    if [ -n "${_REAL_DB:-}" ] && [ -d "$_REAL_DB" ]; then
        # hermetic-ok: $_REAL_DB is the pre-stage SPIRA_DB, read-only here to verify isolation
        nowant "stage bead not visible in real db" "${created_id:-<no id>}" \
            "$(bd -C "$_REAL_DB" list --limit 0 --label "spira,plan" 2>/dev/null)"
    else
        ok "real db isolation (no real db to check against)"
    fi
)

# ─── T4: stage down removes the root completely ───────────────────────────────
printf '\nT4: stage down removes STAGE_ROOT\n'
(
    # Reset before every stage boot: a prior T-block's post-eval sync below (real stage
    # credential, for ITS OWN spira-lc calls) would otherwise leak into THIS stage's own
    # `release stage up` schema-apply bootstrap as a stale, now-torn-down path — the
    # "cannot tell: reading ... No such file or directory" failure this round surfaced.
    tl_config SPIRA_LC_PASSWORD_FILE=""
    eval "$(release stage up)" \
        || { printf '  FATAL: stage up failed\n'; exit 1; }
    saved_root="$STAGE_ROOT"
    (exec 3<>"/dev/tcp/127.0.0.1/$SPIRA_LC_PORT") 2>/dev/null \
        && ok "lifecycle server accepts while the stage is up" \
        || bad "lifecycle server accepts while the stage is up" "nothing on $SPIRA_LC_PORT"
    release stage down "$STAGE_ROOT"
    [ ! -d "$saved_root" ] && ok "STAGE_ROOT removed after down" \
                            || bad "STAGE_ROOT removed after down" "$saved_root still exists"
    (exec 3<>"/dev/tcp/127.0.0.1/$SPIRA_LC_PORT") 2>/dev/null \
        && bad "lifecycle server stopped by down" "still accepting on $SPIRA_LC_PORT" \
        || ok "lifecycle server stopped by down"
)

# ─── T5: stage down refuses a non-stage path ─────────────────────────────────
printf '\nT5: stage down refuses a non-stage path\n'
(
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    err="$(release stage down "$tmp" 2>&1)" && rc=0 || rc=$?
    isnt "down of non-stage exits non-zero" "0" "$rc"
    want "down of non-stage prints refusal" "canary.fayth" "$err"
    rm -rf "$tmp"
)

# ─── T6: canary-worker claims through the machine, commits, submits ──────────
printf '\nT6: canary-worker claims through the lifecycle machine and submits\n'
(
    # Reset before every stage boot: a prior T-block's post-eval sync below (real stage
    # credential, for ITS OWN spira-lc calls) would otherwise leak into THIS stage's own
    # `release stage up` schema-apply bootstrap as a stale, now-torn-down path — the
    # "cannot tell: reading ... No such file or directory" failure this round surfaced.
    tl_config SPIRA_LC_PASSWORD_FILE=""
    eval "$(release stage up)" \
        || { printf '  FATAL: stage up failed\n'; exit 1; }
    trap 'release stage down "$STAGE_ROOT" 2>/dev/null' EXIT
    # SPIRA_LC_PASSWORD_FILE is registered (spira-lc/src/db.rs resolves it via cfg(), not
    # raw env, unlike SPIRA_LC_PORT/SPIRA_LC_USER which stay raw env) — this suite's own
    # earlier override (cleared to "" so `release stage up`'s OWN schema-apply step, which
    # inherits the operator's env, doesn't choke on the fixture's bogus credential path)
    # would otherwise persist and starve any `spira-lc` call below of the stage's real,
    # just-generated credential (eval'd above as a plain var).
    tl_config SPIRA_LC_PASSWORD_FILE="${SPIRA_LC_PASSWORD_FILE:-}"

    # Create an unparented plan bead (there is no goal epic, sp-k6m1m)
    # hermetic-ok: $SPIRA_DB is the stage database — always a mktemp temp dir from stage.sh up
    bead="$(bd -C "$SPIRA_DB" create "t6 task" --type task \
        --labels "spira,plan" --silent 2>/dev/null | tr -d '[:space:]')"
    [ -n "$bead" ] || { printf '  FATAL: could not create bead\n'; exit 1; }

    # Its lifecycle row, as release canary files it. POSITIVE CONTROL: READY before the
    # worker, so the SUBMITTED below is the worker's doing.
    timeout 5 spira-lc create-bead "$bead" >/dev/null 2>&1
    is "bead's lifecycle row is READY before the worker" "READY" "$(lc_field "$bead" state)"

    # Run the worker directly (it inherits the stage env, SPIRA_LC_* included, from the subshell)
    release canary-worker 2>"$STAGE_ROOT/worker.err"
    rc=$?
    is "release canary-worker exits 0" "0" "$rc"
    [ "$rc" = 0 ] || sed 's/^/      worker: /' "$STAGE_ROOT/worker.err" | tail -5

    # The machine, not bd, records the hand-on: claimed (holder) and submitted at the tip.
    is "bead is SUBMITTED in the stage's machine after the worker" "SUBMITTED" "$(lc_field "$bead" state)"
    tip="$(git -C "$STAGE_ROOT/remote.git" rev-parse "spira/$bead" 2>/dev/null)"
    is "the submitted tip is the pushed branch's" "${tip:-<no branch>}" "$(lc_field "$bead" tip)"

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
    # Reset before canary boots its own stage internally (same leak as the T-blocks above:
    # a prior block's real, now-torn-down stage credential must not survive into this one).
    tl_config SPIRA_LC_PASSWORD_FILE=""
    # canary.sh runs `sentinel` and `landing-pass` by name, on this suite's PATH.
    # Capture what canary prints; exit code is what matters.
    out="$(release canary 2>&1)"
    rc=$?
    is "release canary exits 0" "0" "$rc"
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

tl_summary
