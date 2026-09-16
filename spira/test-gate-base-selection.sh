#!/usr/bin/env bash
# test-gate-base-selection.sh — the base trial selects what the branch's diff selects.
#
# Run against the base, a diff-selecting gate command sees an empty diff, selects nothing and
# passes, so every red suite reads as the branch's own. The gate therefore hands both trials
# the branch as the selection head.
# covers: spira/gate.sh spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want(){ [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
MAP="$TMP/repo-map"; HOMEDIR="$TMP/home"; RUNS="$TMP/invocations"
mkdir -p "$RUN/worktree" "$HOMEDIR" "$SH"; : > "$RUNS"
cp "$HERE/gate.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/exclude.sh" "$HERE/skew.sh" "$HERE/yield.sh" "$SH/"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
printf 'base\n' > "$REPO/marker"; git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"; git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
BR=spira/sp-v1
git -C "$REPO" worktree add -q -b "$BR" "$TMP/work" origin/main
printf 'one\n' > "$TMP/work/f1.txt"; git -C "$TMP/work" add -A; git -C "$TMP/work" commit -q -m "feat: sp-v1 — work"

# The command records which ref it was run at and which head it was told to select from, and
# is red at both, so the gate must run the base trial and report base-red.
printf 'repo | %s | push | origin/main |  | %s\n' "$REPO" \
    "echo \"\$SPIRA_GATE_BRANCH \${SPIRA_GATE_SELECT_HEAD:-unset}\" >> $RUNS; exit 1" > "$MAP"

echo "test-gate-base-selection.sh"
out="$(env -i HOME="$HOMEDIR" PATH="/usr/bin:/bin" \
    GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
    SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
    SPIRA_DB="$TMP/nonexistent-db" SPIRA_REPO_MAP="$MAP" SPIRA_GATE_LOG="$TMP/gate.log" \
    SPIRA_VERDICTS="$TMP/verdicts" SPIRA_VERDICT_TTL=600 \
    bash "$SH/gate.sh" "$BR" repo 2>&1)"
is   "both trials ran"                          2 "$(wc -l < "$RUNS" | tr -d ' ')"
want "a command red on both is charged to the base" "reason=base-red" "$out"
is   "the branch trial selects from the branch" "$BR" "$(sed -n 1p "$RUNS" | cut -d' ' -f2)"
base_line="$(sed -n 2p "$RUNS")"
isnt_branch="$(printf '%s' "$base_line" | cut -d' ' -f1)"
[ "$isnt_branch" != "$BR" ] && ok "the second trial ran at the base" || bad "the second trial ran at the base" "$base_line"
is   "and the base trial also selects from the branch" "$BR" "$(printf '%s' "$base_line" | cut -d' ' -f2)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
