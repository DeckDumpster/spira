#!/usr/bin/env bash
#
# test-aeon-prod-dirty.sh — the prod-dirty guard is bound to the aeon's own worktree,
#                           not to the shared harness checkout.
#
#   ./test-aeon-prod-dirty.sh
#
# THE DEFECT THIS GUARDS. An aeon that closes a bead while its own worktree carries
# uncommitted tracked modifications has left unreviewed code invisible to the commit graph.
# The guard must be bound to $WORK (the aeon's own worktree) — not to $SPIRA_REPO (the
# shared harness checkout). Binding it to the shared checkout punishes whichever aeon
# happens to close next for a condition it neither caused nor can fix, producing an
# unbounded requeue loop whenever any stray file appears in the shared checkout.
#
# WHAT THIS SUITE USED TO BE, AND IS NOW. It drove four cases: a close with the aeon's OWN
# worktree dirty (reopened, paths and override named), the same with SPIRA_ALLOW_PROD_DIRTY=1
# (close stands), a dirty SPIRA_REPO (close stands — sp-nqtrg) and a clean baseline. The guard
# itself lives in the verdict's closed branch (aeon verdict.rs, `closed && committed`), and
# since sp-v62vn every session is restricted and hands its bead on only through the work
# verbs, so decide::builder_closed is false for every session and the guard is reached by none.
# The guard now judges the restricted hand-on too (decide::builder_submitted): own-dirty is
# refused back to rework with the paths and override named; the override lets it stand; a
# dirty shared checkout (sp-nqtrg) and a clean baseline both read SUBMITTED.
#
# Driven through the REAL aeon.sh against a real bd on a throwaway fixture, with a shim
# standing in for the model (law-prefer-the-real-dependency). SPIRA_REPO is a separate git
# repository from the bead's target repo, matching production topology where the harness
# checkout and the work repo are different paths.
#
# ISOLATION BETWEEN CASES: after each case the bead is marked spira-poison so subsequent aeon
# runs do not claim it. Only the current case's bead is available for the next aeon.
#
# tier: T2
# covers: aeon/src/*
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# PD_PART: main runs cases 0 and 1, rest runs cases 2 and 3; test-aeon-prod-dirty-rest.sh
# sets rest and sources this file (split for wall time).
PD_PART="${PD_PART:-main}"
. "$HERE/testlib.sh"

# CLEAR ANY INHERITED SHARED FIXTURE before building our own. An aeon session exports
# TESTDB_SHARED=1 with temp dirs that may no longer exist on disk; running testdb_up
# against a stale shared fixture always fails. Unsetting here forces a fresh build for
# this suite without affecting the caller's environment (subshell cannot modify parent).
#
# SPIRA_DB IS ALSO UNSET. The aeon session sets SPIRA_DB to the production database path,
# which has a .beads directory. conf.sh (sourced transitively via testdb.sh) runs
# `bd migrate schema` against any SPIRA_DB that has .beads. The production database uses
# a dolt server that may not be running during a test. Unsetting SPIRA_DB causes conf.sh
# to re-derive it to the default path (~/.local/share/spira/db) which has no .beads, so
# the migration check is skipped. testdb_up then sets SPIRA_DB to the fresh fixture.
unset TESTDB_SHARED TESTDB_NAME TESTDB_DIR TESTDB_BASELINE TESTDB_BIN \
      TESTDB_MODE TESTDB_STARTED_SERVICE SPIRA_DB

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-prod-dirty
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up "aeonproddirty$PD_PART" || { echo "test-aeon-prod-dirty: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# ---- harness repo (SPIRA_REPO) --------------------------------------------------------
# A tracked file that can be dirtied independently of the bead worktree. SPIRA_HOME lives
# inside this repo so conf.sh derives SPIRA_REPO = the git root = this checkout.
# Topology matches production: harness and bead target are separate checkouts.
HARNESS_ORIGIN="$TMP/harness.git"; git init -q --bare -b main "$HARNESS_ORIGIN"
HARNESS="$TMP/harness"; timeout 5 git clone -q "$HARNESS_ORIGIN" "$HARNESS" 2>/dev/null
git -C "$HARNESS" config user.email t@t; git -C "$HARNESS" config user.name t
printf 'harness script v1\n' > "$HARNESS/incident.sh"
git -C "$HARNESS" add incident.sh
git -C "$HARNESS" commit -qm "seed harness"
timeout 5 git -C "$HARNESS" push -q origin main 2>/dev/null

# THE GUARD: set SPIRA_REPO explicitly to the harness checkout so both the test and the
# verdict block address the same path. conf.sh uses ${SPIRA_REPO:-derived}, so an already-
# exported value wins over derivation.
export SPIRA_REPO="$HARNESS"

# ---- bead target repo (separate from SPIRA_REPO) --------------------------------------
ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; timeout 5 git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; timeout 5 git -C "$REPO" push -q origin main 2>/dev/null

# ---- harness setup --------------------------------------------------------------------
# SPIRA_HOME is a subdirectory of HARNESS (a git-tracked repo), so conf.sh derives
# SPIRA_REPO = git root of HARNESS, matching the exported SPIRA_REPO above.
export SPIRA_HOME="$HARNESS/spira-home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SPIRA_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"; tl_config SPIRA_RUN="$SPIRA_RUN"
# The complete fixture declares a non-empty SPIRA_CHAMBER ("/fixture/userhome/.../chamber");
# nothing derives it from SPIRA_HOME any more (sfail round 2, pattern 6) — without this
# the aeon never finds this fixture's builder.fayth at all.
tl_config SPIRA_CHAMBER="$SPIRA_HOME/chamber"
SPIRA_REPO_MAP="$TMP/repo-map"; tl_config SPIRA_REPO_MAP="$SPIRA_REPO_MAP"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
# builder.fayth: excludes spira-poison so beads from completed cases are skipped.
cat > "$SPIRA_HOME/chamber/builder.fayth" <<'FAYTH'
FAYTH_NAME=builder
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,needs-operator"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

# ---- shim: stands in for claude -------------------------------------------------------
# conf.sh replaces $PATH entirely, so a PATH shim silently runs the real model.
BIN="$TMP/bin"; mkdir -p "$BIN"; export TMP HARNESS
tl_config SPIRA_AGENT="$BIN/claude"
# sp-mve9i: the aeon reads its bead's state from the lifecycle row, never bd status; the
# shim's bd close is told to it in lifecycle terms (testlib.sh lc_aeon_mirror).
lc_aeon_mirror "$TMP/lcm"; export PATH="$TMP/lcm:$PATH"
# The model session is restricted (sp-v62vn); the shim is a fixture — testlib
# aeon_fixture_agent, carrying the shared checkout the repo-dirty case writes into.
aeon_fixture_agent "$BIN/claude" HARNESS
command -v aeon >/dev/null 2>&1 \
    || { echo "test-aeon-prod-dirty: aeon is not on PATH — refusing to run the real model" >&2; exit 1; }

# Shim behaviour is driven by $TMP/shim-dirty:
#   clean       — commit only; leave worktree and SPIRA_REPO clean
#   repo-dirty  — commit; then dirty SPIRA_REPO (the shared harness checkout, not $WORK)
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"

# Commit the bead's own work.
printf 'my work\n' >> f
git add f && git -c user.email=a@a -c user.name=aeon commit -qm "$id: the work"

# Optionally leave extra dirt in the worktree or in SPIRA_REPO.
case "$(cat "$TMP/shim-dirty" 2>/dev/null)" in
    own-dirty)
        # Stage a further change in the worktree WITHOUT committing it.
        printf 'staged leftover\n' >> f
        git add f
        ;;
    repo-dirty)
        # Dirty SPIRA_REPO (the shared harness checkout), not the bead worktree.
        # This is the condition that was wrongly causing bead requeues before sp-nqtrg.
        printf 'unexpected local edit\n' >> "$HARNESS/incident.sh"
        ;;
esac

bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
SHIM
chmod +x "$BIN/claude"

# "STAYS CLOSED" UNDER sp-v62vn. The session's bd close reads, in the lifecycle stand-in, as
# the builder's submit; nothing reopened it, so the row reads SUBMITTED (testlib lc_row_state).
not_reopened() {  # not_reopened <case-name> <bead-id>
    is "$1: the session's hand-on stands (SUBMITTED)" "SUBMITTED" "$(lc_row_state "$2")"
}

latest_note() {   # latest_note <bead-id>
    timeout 5 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("notes","") if d else "")' 2>/dev/null
}

# Helper: poison a bead so later aeon runs skip it (isolates cases from each other).
poison_bead() {
    timeout 5 bd -C "$SPIRA_DB" label add "$1" spira-poison >/dev/null 2>&1 || true
}

if [ "$PD_PART" = main ]; then
# ============================================================
echo
echo "CASE 0 (positive control): OWN WORKTREE dirty — the submission is refused, back to rework:"
echo "-----------------------------------------------------------------------"
printf 'own-dirty' > "$TMP/shim-dirty"
b1="$(timeout 5 bd -C "$SPIRA_DB" create --title "test: own worktree dirty" --type task \
        -l "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan},repo:fixture" 2>/dev/null | grep -oE 'sp-[a-z0-9-]+')"
[ -n "$b1" ] || { bad "case 0 bead created" "(bead-create failed)"; true; }
unset SPIRA_ALLOW_PROD_DIRTY
aeon --home "$SPIRA_HOME" builder >/dev/null 2>&1 || true
nowant "own-dirty: the hand-on is refused (not SUBMITTED)" "SUBMITTED" "$(lc_row_state "$b1")"
note1="$(latest_note "$b1")"
want "own-dirty: note names the modified path" "f" "$note1"
want "own-dirty: note names the override variable" "SPIRA_ALLOW_PROD_DIRTY" "$note1"
poison_bead "$b1"

echo
echo "CASE 1: SPIRA_REPO dirty, own worktree clean — the hand-on stands (regression for sp-nqtrg):"
echo "-----------------------------------------------------------------------"
printf 'repo-dirty' > "$TMP/shim-dirty"
b2="$(timeout 5 bd -C "$SPIRA_DB" create --title "test: repo dirty only" --type task \
        -l "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan},repo:fixture" 2>/dev/null | grep -oE 'sp-[a-z0-9-]+')"
[ -n "$b2" ] || { bad "case 1 bead created" "(bead-create failed)"; true; }
unset SPIRA_ALLOW_PROD_DIRTY
aeon --home "$SPIRA_HOME" builder >/dev/null 2>&1 || true
not_reopened "repo-dirty" "$b2"
# Restore HARNESS so it does not affect later cases.
git -C "$HARNESS" checkout -q -- incident.sh 2>/dev/null || true
poison_bead "$b2"
fi

if [ "$PD_PART" = rest ]; then
# ============================================================
echo
echo "CASE 2: both clean — the hand-on stands (baseline):"
echo "-----------------------------------------------------------------------"
printf 'clean' > "$TMP/shim-dirty"
b3="$(timeout 5 bd -C "$SPIRA_DB" create --title "test: clean" --type task \
        -l "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan},repo:fixture" 2>/dev/null | grep -oE 'sp-[a-z0-9-]+')"
[ -n "$b3" ] || { bad "case 2 bead created" "(bead-create failed)"; true; }
unset SPIRA_ALLOW_PROD_DIRTY
aeon --home "$SPIRA_HOME" builder >/dev/null 2>&1 || true
not_reopened "clean" "$b3"
poison_bead "$b3"

echo
echo "CASE 3: own worktree dirty with SPIRA_ALLOW_PROD_DIRTY=1 — the hand-on stands (override):"
echo "-----------------------------------------------------------------------"
printf 'own-dirty' > "$TMP/shim-dirty"
b4="$(timeout 5 bd -C "$SPIRA_DB" create --title "test: own dirty with override" --type task \
        -l "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan},repo:fixture" 2>/dev/null | grep -oE 'sp-[a-z0-9-]+')"
[ -n "$b4" ] || { bad "case 3 bead created" "(bead-create failed)"; true; }
SPIRA_ALLOW_PROD_DIRTY=1 aeon --home "$SPIRA_HOME" builder >/dev/null 2>&1 || true
fi
not_reopened "override (despite dirty worktree)" "$b4"

echo
tl_summary
