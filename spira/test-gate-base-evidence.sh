#!/usr/bin/env bash
# test-gate-base-evidence.sh — a red base names its own red suites and carries its own output,
# and excuses only the suites that are red on it too.
# covers: spira/gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want(){ [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"; HOMEDIR="$TMP/home"
mkdir -p "$HOMEDIR" "$SH"
cp "$HERE/gate.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/exclude.sh" "$HERE/skew.sh" "$HERE/yield.sh" "$HERE/suite-covers.sh" "$SH/"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
printf 'base\n' > "$REPO/marker"; git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"; git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
BR=spira/sp-v1
git -C "$REPO" worktree add -q -b "$BR" "$TMP/work" origin/main
printf 'one\n' > "$TMP/work/f1.txt"; git -C "$TMP/work" add -A; git -C "$TMP/work" commit -q -m "feat: sp-v1 — work"

# gate_with <printed at the branch> <printed at the base>: both trials exit 1.
gate_with() {
    local run="$TMP/run.$RANDOM$RANDOM" map="$TMP/map.$RANDOM$RANDOM"
    mkdir -p "$run/worktree"
    printf '%s\n' "$1" > "$run/at-branch"; printf '%s\n' "$2" > "$run/at-base"
    printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" \
        "if [ \"\$SPIRA_GATE_BRANCH\" = \"$BR\" ]; then cat $run/at-branch; else cat $run/at-base; fi; exit 1" > "$map"
    env -i HOME="$HOMEDIR" PATH="/usr/bin:/bin" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$run" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_REPO_MAP="$map" SPIRA_GATE_LOG="$run/gate.log" \
        SPIRA_VERDICTS="$run/verdicts" SPIRA_VERDICT_TTL=600 \
        bash "$SH/gate.sh" "$BR" repo 2>&1
}

echo "test-gate-base-evidence.sh"

echo "the same suite red on both: charged to the base, named, with the base's own output"
out="$(gate_with '  test-shared.sh                   RED     rc=1 after 2s' \
                 $'  test-shared.sh                   RED     rc=1 after 2s\nBASE-ONLY-EVIDENCE')"
want   "base-red verdict"                 "reason=base-red"       "$out"
want   "the suite is named, not -"        "suite=test-shared.sh"  "$out"
want   "the base's own output is carried" "BASE-ONLY-EVIDENCE"    "$out"

echo "a base trial with only timeouts is NO_VERDICT, not BASE_FAIL"
out="$(gate_with '  test-slow.sh                     TIMEOUT after 600s' \
                 '  test-slow.sh                     TIMEOUT after 600s')"
want   "base-timeout verdict"             "reason=base-timeout"   "$out"
want   "VERDICT is NO_VERDICT"            "VERDICT=NO_VERDICT"    "$out"
want   "timed suite is named"             "suite=test-slow.sh"    "$out"
nowant "not charged as BASE_FAIL"         "VERDICT=BASE_FAIL"     "$out"

echo "a base with both genuine failures and timeouts is still BASE_FAIL"
out="$(gate_with $'  test-real.sh                     RED     rc=1 after 2s\n  test-slow.sh                     TIMEOUT after 600s' \
                 $'  test-real.sh                     RED     rc=1 after 2s\n  test-slow.sh                     TIMEOUT after 600s')"
want   "base-red verdict retained"        "reason=base-red"       "$out"
want   "VERDICT is BASE_FAIL"             "VERDICT=BASE_FAIL"     "$out"

echo "a suite red only on the branch is the branch's, even while the base is red elsewhere"
out="$(gate_with $'  test-shared.sh                   RED     rc=1 after 2s\n  test-mine.sh                     RED     rc=1 after 2s' \
                 '  test-shared.sh                   RED     rc=1 after 2s')"
want   "branch-red verdict"               "reason=branch-red"     "$out"
nowant "not excused as base-red"          "reason=base-red"       "$out"
want   "the branch's own suite is named"  "suite=test-mine.sh"    "$out"

echo "neither trial names a suite: still the base's, with -"
out="$(gate_with 'lint said no' 'lint said no')"
want   "unnamed base red is base-red"     "reason=base-red"       "$out"
want   "suite is -"                       "suite=-"               "$out"

echo "a base trial returning NV (exit 75) is NO_VERDICT, not BASE_FAIL"
# The batch converts a repeat-refused run (exit 2) to exit 75 via the gate command's
# case clause. A base trial that returns NV means we cannot confirm the base is broken;
# it must not charge the branch or hold the repository as BASE_FAIL.
# POSITIVE CONTROL: the case above (both exit 1, unnamed) still produces BASE_FAIL, so
# this silence is not from a guard that never fires.
_run_nv="$TMP/run.nv"; _map_nv="$TMP/map.nv"
mkdir -p "$_run_nv/worktree"
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" \
    "if [ \"\$SPIRA_GATE_BRANCH\" = \"$BR\" ]; then exit 1; else exit 75; fi" > "$_map_nv"
out_nv="$(env -i HOME="$HOMEDIR" PATH="/usr/bin:/bin" \
    GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
    SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$_run_nv" \
    SPIRA_DB="$TMP/nonexistent-db" SPIRA_REPO_MAP="$_map_nv" SPIRA_GATE_LOG="$_run_nv/gate.log" \
    SPIRA_VERDICTS="$_run_nv/verdicts" SPIRA_VERDICT_TTL=600 \
    bash "$SH/gate.sh" "$BR" repo 2>&1)"
want   "base returning NV gives NO_VERDICT"  "VERDICT=NO_VERDICT" "$out_nv"
nowant "and is not charged as BASE_FAIL"     "VERDICT=BASE_FAIL"  "$out_nv"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
