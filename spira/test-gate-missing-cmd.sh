#!/usr/bin/env bash
#
# test-gate-missing-cmd.sh — gate.sh returns a distinct config-error verdict when the
# gate command names a bash <path> absent from the base, instead of BASE_FAIL.
#
#   ./test-gate-missing-cmd.sh
#
# WHY THIS SUITE EXISTS. A gate command naming a file absent from the base fails every
# branch: the branch trial exits 127 (no such file), and the base trial also exits 127,
# so gate.sh reports BASE_FAIL — "the base fails its own gate" — sending diagnosis toward
# a failing suite that does not exist. This was the defect in sp-lkzl: scratch-fence.sh
# was wired into the repo-map gate command before the file existed on origin/main, and
# the result was 22 branches with 0 movement across one landing pass.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION. A gate that returns config-error on every
# call is indistinguishable from one that catches only the missing-file case. The positive
# control adds the file to the base and requires gate.sh to reach and run the CMD — if it
# still returns config-error after the file is present, the check is wrong.
#
# defect: sp-lkzl
# host-reason: creates scratch git repos to test gate.sh; no container-hosted state
# covers: spira/gate.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
MAP="$TMP/repo-map"
mkdir -p "$RUN/worktree" "$SH"

# THE GATE UNDER TEST IS A COPY. lib.sh, conf.sh, exclude.sh, skew.sh, yield.sh and
# suite-covers.sh travel with it because gate.sh fails closed on their absence and
# lib.sh sources suite-covers.sh on startup.
cp "$HERE/gate.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/exclude.sh" \
   "$HERE/skew.sh" "$HERE/yield.sh" "$HERE/suite-covers.sh" "$SH/"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
# base: a minimal harness directory but NOT the file the CMD will name
mkdir -p "$REPO/tools"
printf 'base\n' > "$REPO/marker"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin

# A branch with a trivial change (does not add the CMD's file)
BR=repo/sp-t1
W="$TMP/work"
git -C "$REPO" worktree add -q -b "$BR" "$W" origin/main
printf 'change\n' > "$W/f.txt"
git -C "$W" add -A; git -C "$W" commit -q -m "change"

# Gate command names a file that does NOT exist on origin/main
GATE_FILE="tools/check.sh"
printf 'repo | %s | push | origin/main |  | bash %s\n' "$REPO" "$GATE_FILE" > "$MAP"

gate() {
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb \
        SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_REPO_MAP="$MAP" \
        SPIRA_GATE_LOCK_WAIT=10 \
        bash "$SH/gate.sh" "$BR" "repo" 2>&1
}

echo "test-gate-missing-cmd.sh"
echo ""
echo "--- SEEN RED: gate command names absent file ---"

out="$(gate)"; rc=$?

# Gate must not pass. Must name the file. Must NOT say BASE_FAIL or base-red.
is   "SEEN RED: gate does not pass" "1" "$([ "$rc" -ne 0 ] && echo 1 || echo 0)"
want "SEEN RED: names the missing file" "$GATE_FILE" "$out"
nowant "SEEN RED: does not say BASE_FAIL" "BASE_FAIL" "$out"
nowant "SEEN RED: does not say base-red reason" "reason=base-red" "$out"

echo ""
echo "--- SEEN GREEN: gate command names present file (positive control) ---"

# Add the file to origin/main so the preflight passes, then create a new branch
RUNS="$TMP/runs"
: > "$RUNS"
printf '#!/usr/bin/env bash\nprintf "ran\\n" >> %s\necho ok\n' "$RUNS" > "$REPO/$GATE_FILE"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "add check.sh"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin

# New branch from updated main
BR2=repo/sp-t2
W2="$TMP/work2"
git -C "$REPO" worktree add -q -b "$BR2" "$W2" origin/main
printf 'change2\n' > "$W2/f2.txt"
git -C "$W2" add -A; git -C "$W2" commit -q -m "change2"

# Update CMD to use the now-present file as the gate command
printf 'repo | %s | push | origin/main |  | bash %s\n' "$REPO" "$GATE_FILE" > "$MAP"

out2="$(env -i PATH="$PATH" HOME="$TMP" TERM=dumb \
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_REPO_MAP="$MAP" \
    SPIRA_GATE_LOCK_WAIT=10 \
    bash "$SH/gate.sh" "$BR2" "repo" 2>&1)"; rc2=$?

is   "SEEN GREEN: gate passes when file is present" "0" "$rc2"
runs="$(wc -l < "$RUNS" | tr -d ' ')"
is   "SEEN GREEN: CMD was actually executed (ran file counted)" "1" "$runs"
nowant "SEEN GREEN: no config-error in output" "configuration error" "$out2"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
