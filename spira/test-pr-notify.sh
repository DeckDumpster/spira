#!/usr/bin/env bash
#
# test-pr-notify.sh — pr-notify.sh's GREEN/RED/skip classifier, direct over its own JSON
# seam, plus one T2 case for the git+gh branch-without-PR check the classifier alone cannot
# cover.
#
#   ./test-pr-notify.sh
#
# WHAT IT HOLDS
#
#   1. POSITIVE CONTROL FIRST. A green PR and a red PR in the same JSON prove the classifier
#      fires before any absence assertion is trusted.
#   2. ACTIONABLE KEYWORDS. Green PRs emit lines containing ⚠; red PRs emit lines containing
#      FAIL. Both are in the default SPIRA_ACTIONABLE set, so watchd notify picks them up.
#   3. PENDING AND NO-CHECK PRs ARE SKIPPED. A PR whose checks have not completed, or that
#      carries none, is neither green nor red.
#   4. SILENT WHEN NOTHING TO REPORT. An empty PR list emits nothing.
#   5. LOG VS SHOW. _emit writes to the watcher log by default and appends on a second call;
#      under --show it prints to stdout and never touches the log.
#   6. BRANCHES WITH NO PR (the one T2 case). A spira/* branch ahead of base with no open PR
#      emits ⚠ BRANCH; one with an open PR does not; a push/hold-mode repo is never scanned.
#
# THE CLASSIFIER (_PR_PY) IS ALREADY A PURE FUNCTION OF STDIN JSON — the fixture git repos
# and stubbed `gh` binary this suite used to build for every case existed only to hand it
# that JSON by a longer road. Rendering/install checks for the unit move to
# test-units-lint.sh (D8); this suite calls _PR_PY directly for everything the git+gh path
# does not itself decide.
#
# tier: T1
# covers: spira/pr-notify.sh UC-operator-channel-38
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

has()   { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "$2" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1" "$2" ;; *) ok "$1" ;; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home" "$TMP/run"

# ALL VALUES PINNED TO NON-DEFAULTS so sourcing pr-notify.sh cannot read the operator's own
# configuration (law-gates-run-in-a-clean-environment).
CONF="$TMP/spira.conf"
RUN="$TMP/run"
printf 'SPIRA_RUN = %s\n' "$RUN" > "$CONF"
export SPIRA_CONF="$CONF" HOME="$TMP/home"

# SOURCEABLE, AND SILENT WHEN IT IS (pr-notify.sh's own guard): this reaches _PR_PY, _emit
# and _scan_repo without triggering a live repo-map scan.
# shellcheck disable=SC1090
. "$HERE/pr-notify.sh" ""

classify() { python3 -c "$_PR_PY" "$1"; }   # classify <repo-label> — JSON on stdin

echo "test-pr-notify.sh"

# ====================================================================================
# 1. POSITIVE CONTROL — the matcher can find green and red PRs before any absence
#    assertion is trusted.
# ====================================================================================
echo
echo "positive control: green and red PRs are found"

out="$(classify managed <<'JSON'
[
  {"number":10,"title":"Green PR","headRefName":"feature/a","statusCheckRollup":[
    {"status":"COMPLETED","conclusion":"SUCCESS","name":"ci"}]},
  {"number":11,"title":"Red PR","headRefName":"feature/b","statusCheckRollup":[
    {"status":"COMPLETED","conclusion":"FAILURE","name":"ci"}]}
]
JSON
)"
has "green PR emits ⚠"      "$out" "⚠"
has "green PR number"        "$out" "#10"
has "green PR title"         "$out" "Green PR"
has "red PR emits FAIL"      "$out" "FAIL"
has "red PR number"          "$out" "#11"
has "red PR title"           "$out" "Red PR"
has "repo label is present"  "$out" "[managed]"

# ====================================================================================
# 2. ACTIONABLE KEYWORDS — ⚠ for green, FAIL for red, and never crossed.
# ====================================================================================
echo
echo "actionable keywords"
has "green uses ⚠ (in SPIRA_ACTIONABLE)"  "$out" "⚠ GREEN #10"
has "red uses FAIL (in SPIRA_ACTIONABLE)" "$out" "FAIL RED #11"
hasnt "green does not use FAIL"           "$out" "FAIL GREEN"
hasnt "red does not use ⚠ GREEN"          "$out" "⚠ GREEN #11"

# ====================================================================================
# 3. PENDING, NULL-CHECK AND EMPTY-CHECK PRs ARE SKIPPED.
# ====================================================================================
echo
echo "pending and no-check PRs are skipped"
out="$(classify repo <<'JSON'
[
  {"number":40,"title":"Pending PR","headRefName":"p","statusCheckRollup":[
    {"status":"IN_PROGRESS","conclusion":null,"name":"ci"}]},
  {"number":41,"title":"No-check PR","headRefName":"q","statusCheckRollup":null},
  {"number":42,"title":"Empty-check PR","headRefName":"r","statusCheckRollup":[]}
]
JSON
)"
is "nothing emitted for pending/no-check/empty-check PRs" "" "$out"

# ====================================================================================
# 4. SILENT WHEN NOTHING TO REPORT — an empty PR list, or unparseable JSON, emits nothing.
# ====================================================================================
echo
echo "silent when nothing to report"
is "empty PR list → no output"   "" "$(classify repo <<< '[]')"
is "unparseable JSON → no output, no crash" "" "$(classify repo <<< 'not json')"

# ====================================================================================
# 5. LOG VS SHOW — _emit writes to the watcher log by default and appends; --show prints to
#    stdout and never touches the log.
# ====================================================================================
echo
echo "log append mode and --show"
LOGFILE="$RUN/watchd/pr-notify.log"
rm -f "$LOGFILE"
_show=0
_emit "⚠ GREEN #20 Appended PR [managed]"
is "log file was created"      "yes" "$([ -f "$LOGFILE" ] && echo yes || echo no)"
has "log contains the finding" "$(cat "$LOGFILE" 2>/dev/null)" "#20"
_emit "⚠ GREEN #21 Second PR [managed]"
is "a second _emit appends (2 lines)" "2" "$(wc -l < "$LOGFILE" | tr -d ' ')"

_show=1
out="$(_emit "FAIL RED #30 Show PR [managed]")"
has "show mode prints to stdout" "$out" "#30"
is "show mode does not touch the log" "2" "$(wc -l < "$LOGFILE" | tr -d ' ')"
_show=0

# ====================================================================================
# 6. BRANCHES WITH NO PR — the one T2 case: a real git remote and a stubbed gh, because
#    this property lives in _scan_repo's git plumbing, not in the JSON classifier.
# ====================================================================================
echo
echo "branches with no PR (T2: real git, stubbed gh)"

BRANCH_REPO="$TMP/branch-repo"
git init --quiet -b main "$BRANCH_REPO" 2>/dev/null \
    || { git init --quiet "$BRANCH_REPO" && git -C "$BRANCH_REPO" checkout -qb main 2>/dev/null; }
git -C "$BRANCH_REPO" config user.email "test@example.com"
git -C "$BRANCH_REPO" config user.name  "Test"
printf 'base\n' > "$BRANCH_REPO/readme"
git -C "$BRANCH_REPO" add readme
git -C "$BRANCH_REPO" commit -q -m "base"
BARE="$TMP/bare-remote"
git clone --quiet --bare "$BRANCH_REPO" "$BARE"
git -C "$BRANCH_REPO" remote add origin "$BARE"
git -C "$BRANCH_REPO" fetch --quiet origin
git -C "$BRANCH_REPO" checkout -q -b spira/test-bead
printf 'aeon work\n' > "$BRANCH_REPO/work"
git -C "$BRANCH_REPO" add work
git -C "$BRANCH_REPO" commit -q -m "spira/test-bead: work"
git -C "$BRANCH_REPO" push -q origin spira/test-bead
git -C "$BRANCH_REPO" checkout -q main
git -C "$BRANCH_REPO" fetch --quiet origin

REPO_MAP="$TMP/repo-map"
printf '%s|%s|pr\n'   "branchrepo" "$BRANCH_REPO" >  "$REPO_MAP"
printf '%s|%s|push\n' "harness"    "$BRANCH_REPO" >> "$REPO_MAP"
printf '%s|%s|hold\n' "held"       "$BRANCH_REPO" >> "$REPO_MAP"

GH_LOG="$TMP/gh.log"
GH_BIN="$TMP/gh-bin"; mkdir -p "$GH_BIN"
cat > "$GH_BIN/gh" <<'GHEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_LOG:-/dev/null}"
case "$*" in
    *"--head "*)          echo "${GH_BRANCH_HAS_PR:-0}" ;;
    *"--state open --json"*) echo "[]" ;;
    *) echo "[]" ;;
esac
GHEOF
chmod +x "$GH_BIN/gh"

run() {  # run <args...> -> pr-notify.sh in a clean env; stdout in $TMP/out
    env -i HOME="$TMP/home" PATH="$PATH" \
        SPIRA_PATH="$GH_BIN" SPIRA_CONF="$CONF" SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_REPO="$TMP/empty-repo" GH_LOG="$GH_LOG" \
        GH_BRANCH_HAS_PR="${GH_BRANCH_HAS_PR:-0}" \
        bash "$HERE/pr-notify.sh" "$@" > "$TMP/out" 2>"$TMP/err"
}

GH_BRANCH_HAS_PR=0
run --show
out="$(cat "$TMP/out")"
has "spira/* branch with no PR emits ⚠ BRANCH" "$out" "⚠ BRANCH spira/test-bead"
has "carries repo label"                        "$out" "[branchrepo]"
hasnt "push-mode repo is not scanned"           "$out" "[harness]"
hasnt "hold-mode repo is not scanned"           "$out" "[held]"

GH_BRANCH_HAS_PR=1
run --show
hasnt "branch with a PR is not reported" "$(cat "$TMP/out")" "⚠ BRANCH spira/test-bead"

tl_summary
