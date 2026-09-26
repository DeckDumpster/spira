#!/usr/bin/env bash
# test-gate-touched.sh — gate-touched.sh: coverage-based landing gate suite selector.
#
# tier: T1
# covers: spira/gate-touched.sh spira/select.sh UC-gate-verdict-08 UC-gate-verdict-09
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
iszero()  { [ "$2" = 0 ]    && ok "$1" || bad "$1" "expected 0, got $2"; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

TOUCHED="$HERE/gate-touched.sh"

# ---------------------------------------------------------------------------
# FIXTURE
# test-a.sh:    covers spira/changed.sh — selected when changed.sh changes.
# test-b.sh:    covers spira/other.sh   — NOT selected (other.sh not in diff).
# test-c.sh:    no covers               — always-run.
# test-meta.sh: covers spira/test-*.sh  — selected when any test file changes.
# ---------------------------------------------------------------------------
R="$TMP/repo"; git init -q -b main "$R"; mkdir -p "$R/spira"
printf '#!/usr/bin/env bash\n# covers: spira/changed.sh\nexit 0\n' > "$R/spira/test-a.sh"
printf '#!/usr/bin/env bash\n# covers: spira/other.sh\nexit 0\n'   > "$R/spira/test-b.sh"
printf '#!/usr/bin/env bash\nexit 0\n'                              > "$R/spira/test-c.sh"
printf '#!/usr/bin/env bash\n# covers: spira/test-*.sh\nexit 0\n'  > "$R/spira/test-meta.sh"
printf 'x\n' > "$R/spira/changed.sh"
printf 'x\n' > "$R/spira/other.sh"
git -C "$R" add -A; git -C "$R" commit -q -m base

# Branch changes changed.sh and adds test-a2.sh (also covers changed.sh).
git -C "$R" checkout -q -b br
printf 'y\n' >> "$R/spira/changed.sh"
printf '#!/usr/bin/env bash\n# covers: spira/changed.sh\nexit 0\n' > "$R/spira/test-a2.sh"
git -C "$R" add -A; git -C "$R" commit -q -m "change changed.sh, add test-a2"

sel() {
    (cd "$R" && SPIRA_GATE_REPO="$R" SPIRA_BATCH_SUITE_DIR="$R/spira" \
        bash "$TOUCHED" main br 2>/dev/null \
        | sort | tr '\n' ' ' | sed 's/ $//')
}

echo "test-gate-touched.sh"
echo

# ---------------------------------------------------------------------------
# POSITIVE CONTROL: coverage-based selection finds what it should, and does
# not find what it should not. This proves the mechanism works before checking
# absent-suite behavior below.
# ---------------------------------------------------------------------------
echo "Part A: coverage-based selection on branch tree"
a_sel="$(sel)"
want    "A1: covering suite selected"           "test-a.sh"   "$a_sel"
want    "A2: branch-added suite selected"       "test-a2.sh"  "$a_sel"
want    "A3: always-run suite selected"         "test-c.sh"   "$a_sel"
nowant "A4: unrelated suite not selected"      "test-b.sh"   "$a_sel"
want    "A5: meta suite selected (test file in diff)" "test-meta.sh" "$a_sel"

# ---------------------------------------------------------------------------
# BASE TREE: a suite added by the branch is absent from the base tree's corpus
# and therefore absent from the selection — select.sh's corpus is built from
# SUITE_DIR's current contents.
# ---------------------------------------------------------------------------
echo
echo "Part B: suite added by branch is absent on base tree"
git -C "$R" checkout -q main
base_sel="$(cd "$R" && SPIRA_GATE_REPO="$R" SPIRA_BATCH_SUITE_DIR="$R/spira" \
    bash "$TOUCHED" main br 2>/dev/null | sort | tr '\n' ' ' | sed 's/ $//')"
nowant "B1: branch-added suite absent on base" "test-a2.sh" "$base_sel"
want    "B2: base suite still selected on base" "test-a.sh"  "$base_sel"
git -C "$R" checkout -q br

# ---------------------------------------------------------------------------
# SPIRA_GATE_FILES: when set, gate-touched.sh uses the pre-computed file list
# rather than computing the diff from BASE/HEAD (HEAD is ignored).
# ---------------------------------------------------------------------------
echo
echo "Part C: SPIRA_GATE_FILES pre-computed file list"
FLIST="$TMP/flist"
printf 'spira/changed.sh\n' > "$FLIST"

fsel="$(cd "$R" && SPIRA_GATE_FILES="$FLIST" SPIRA_GATE_REPO="$R" SPIRA_BATCH_SUITE_DIR="$R/spira" \
    bash "$TOUCHED" main br 2>/dev/null | sort | tr '\n' ' ' | sed 's/ $//')"
want    "C1: GATE_FILES: covering suite selected"     "test-a.sh"  "$fsel"
nowant "C2: GATE_FILES: branch-added suite excluded" "test-a2.sh" "$fsel"
nowant "C3: GATE_FILES: unrelated suite excluded"    "test-b.sh"  "$fsel"

# ---------------------------------------------------------------------------
# SPIRA_GATE_EJECTED_SUITES (UC-gate-verdict-09, sp-px6ng): a suite named here is
# selected regardless of the diff. Folded in from test-reopen-queue-eject.sh, which
# built a second git fixture to prove the same selector on the same code path.
# ---------------------------------------------------------------------------
echo
echo "Part D: SPIRA_GATE_EJECTED_SUITES"
sel_ej() {
    (cd "$R" && SPIRA_GATE_EJECTED_SUITES="$1" SPIRA_GATE_REPO="$R" SPIRA_BATCH_SUITE_DIR="$R/spira" \
        bash "$TOUCHED" main br 2>/dev/null | sort | tr '\n' ' ' | sed 's/ $//')
}
want   "D1: ejected suite added though not covered by the diff" "test-b.sh" "$(sel_ej test-b.sh)"
want   "D2: CSV ejected suites: both included (1)" "test-a.sh" "$(sel_ej "test-a.sh,test-b.sh")"
want   "D2: CSV ejected suites: both included (2)" "test-b.sh" "$(sel_ej "test-a.sh,test-b.sh")"
is     "D3: dedup — ejecting an already-covered suite changes nothing" \
       "$a_sel" "$(sel_ej test-a.sh)"
nowant "D4: an ejected suite absent from the tree is silently skipped" "test-gone.sh" "$(sel_ej test-gone.sh)"

echo
tl_summary
