#!/usr/bin/env bash
#
# test-cockpit-unsent.sh — the unsent backlog counts across repositories, not just the first.
#
#   ./test-cockpit-unsent.sh
#
# THE FAILURE THIS SUITE EXISTS FOR. SP_BRANCH_DONE was echoed inside the per-repo loop and
# write_snapshot's first-wins dedup pinned it to the first repository's count, which was always
# zero because brain has no closed-bead branches. And SP_UNSENT counted every ref under
# refs/heads/spira/* regardless of whether the suffix resolved to a bead, so a stray ref was
# a permanent +1 on a figure whose whole purpose is to trend to zero.
#
# BOTH DEFECTS ARE INVISIBLE TO A SINGLE-REPO TEST. The per-repo overwrite only fires when a
# second repository contributes differently from the first, and a non-bead branch only matters
# when the probe distinguishes it from real work. This suite drives a TWO-repo fixture and
# plants both shapes.
#
# defect: sp-884p
# covers: spira/cockpit.sh
# scar: SP_BRANCH_DONE was overwritten per-repo so only the first repository's zero survived; SP_UNSENT counted every ref under refs/heads/spira/* regardless of whether the suffix resolved to a bead.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require cockpit-unsent
testdb_up cockpit-unsent || exit 1

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# Git identity for fixture commits — required in the container (no ~/.gitconfig).
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
BASE_PATH="$PATH"
# conf.sh (sourced by testdb.sh) knows where bd lives; pass it through so the probe can find it.
BD_PATH="${SPIRA_PATH:-}"
# Resolve the binary testdb_up selected (bd-embedded for embedded mode, bd for server mode)
# to an absolute path now, while PATH is still expanded to include TESTDB_BIN. An absolute
# path is required because env -i strips PATH; a bare command name would fail to resolve.
# Using SPIRA_BD (set by testdb_up) is the correct choice: it is already the binary that can
# open the testdb. The original `command -v bd` found the CGO_ENABLED=0 binary, which cannot
# open the embedded store and caused conf.sh's `bd migrate schema` to fail at startup —
# killing the probe before it emitted a single key (defect sp-gzufi).
REAL_BD="$(command -v "${SPIRA_BD:-bd}" 2>/dev/null)"
[ -n "$REAL_BD" ] || { echo "SKIP cockpit-unsent: no bd binary" >&2; exit 77; }

# Two git repos. `alpha` is the home repo (listed first by spira_repos), `beta` is the second.
ALPHA="$TMP/alpha"; BETA="$TMP/beta"
for r in "$ALPHA" "$BETA"; do
    git init -q "$r"                                   # hermetic-ok: throwaway fixture repos in $TMP
    git -C "$r" commit --allow-empty -m "init" -q      # hermetic-ok: seed commit for the fixture
done

MAP="$TMP/repo-map"
cat > "$MAP" <<MAP
# name | path | land | base | format | gate
alpha | $ALPHA | push | origin/main | |
beta  | $BETA  | push | origin/main | |
MAP
RUN="$TMP/run"; mkdir -p "$RUN"

# A closed bead in BETA, with a branch in beta. Nothing in alpha.
testdb_seed <<'JSONL'
{"id":"sp-aaa","title":"work in beta","status":"closed","labels":["spira","plan","repo:beta"]}
{"id":"sp-bbb","title":"open work in beta","status":"in_progress","labels":["spira","plan","repo:beta"]}
JSONL

git -C "$BETA" checkout -q -b spira/sp-aaa
git -C "$BETA" commit --allow-empty -m "sp-aaa work" -q
git -C "$BETA" checkout -q -b spira/sp-bbb
git -C "$BETA" commit --allow-empty -m "sp-bbb work" -q
git -C "$BETA" checkout -q main 2>/dev/null || git -C "$BETA" checkout -q master

# A non-bead branch in alpha — its suffix resolves to no bead.
git -C "$ALPHA" checkout -q -b spira/tmp-stray
git -C "$ALPHA" commit --allow-empty -m "stray" -q
git -C "$ALPHA" checkout -q main 2>/dev/null || git -C "$ALPHA" checkout -q master

# Run the probe in a minimal environment. cockpit.sh once without INVOCATION_ID prints keys
# to stdout rather than writing a snapshot, which is what we want to parse.
out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$ALPHA" SPIRA_HOME_REPO=alpha \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$REAL_BD" \
    SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_PATH="$BD_PATH" \
    bash "$HERE/cockpit.sh" once 2>/dev/null)"

# Extract keys from output.
val() { printf '%s' "$out" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

echo "unsent backlog — two-repo fixture:"

# ======================================================================================
# DEFECT 1: SP_BRANCH_DONE must be the sum across ALL repos, not just the first.
# alpha has 0 closed-bead branches, beta has 1 (sp-aaa is closed). The sum is 1.
is "SP_BRANCH_DONE counts across repos" "1" "$(val SP_BRANCH_DONE)"

# ======================================================================================
# DEFECT 2: a non-bead branch is not counted in SP_UNSENT.
# beta has 2 bead branches (sp-aaa, sp-bbb). alpha has 0 bead branches (sp/tmp-stray is not
# a bead). Total bead-backed unsent: 2.
is "SP_UNSENT counts only bead-backed branches" "2" "$(val SP_UNSENT)"

# The stray is reported separately.
is "SP_UNADOPTED counts non-bead branches" "1" "$(val SP_UNADOPTED)"

# ======================================================================================
# THE POSITIVE CONTROL: the probe found SOMETHING. An empty output would pass all the
# negative assertions above, which is the shape law-absence-needs-a-positive-control warns
# about.
want "output contains SP_AT" "SP_AT=" "$out"
want "output contains SP_UNSENT" "SP_UNSENT=" "$out"

echo
printf 'test-cockpit-unsent: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
