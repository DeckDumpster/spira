#!/usr/bin/env bash
# test-landing-pass.sh — three properties of landing-pass and its helpers.
#
#   1. landing.sh skips gate.sh entirely for pr-mode repos (positive control: gate IS
#      called for push-mode repos with a closed bead).
#   2. land_pr opens the pull request against master, not main, when the base is origin/master.
#   3. land_pr does not open a second PR when an open PR already carries every commit
#      on the branch (positive control shows ghq pr create IS called when no dup exists).
#
# defect: sp-n971b
# covers: spira/landing-pass spira/pr-pass-branch.sh spira/lib.sh spira/landing.sh
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

. "$HERE/testdb.sh"
testdb_require test-landing-pass
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing-pass || {
    printf 'SKIP test-landing-pass: no bd engine available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

SH="$TMP/spira"; RUN="$TMP/run"
mkdir -p "$SH" "$RUN/worktree" "$RUN/submitted" "$RUN/landstate"
cp "$HERE"/*.sh "$SH/"

stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }

stub mail.sh 'exit 0'
stub confine.sh 'exit 0'
stub incident.sh 'echo sp-fake; exit 0'

# GATE SPY: records each invocation by repo name so the no-gate test can verify.
stub gate.sh 'printf "gate: branch=%s repo=%s\n" "$1" "${2:-?}" >> "$SPIRA_RUN/gate-calls"
echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'

B() { bd -C "$SPIRA_DB" "$@"; }

testdb_reset

# ──────────────────────────────────────────────────────────────────────────────
# TEST 1: no-gate for pr-mode (positive control first)
#
# Positive control: push-mode repo with a closed bead → gate.sh IS invoked.
# Test: pr-mode repo with a closed bead → gate.sh is NOT invoked.
# ──────────────────────────────────────────────────────────────────────────────
PUSH_REMOTE="$TMP/push-remote.git"; PUSH_REPO="$TMP/push-repo"
PR_REMOTE="$TMP/pr-remote.git";     PR_REPO="$TMP/pr-repo"

git init -q --bare -b main "$PUSH_REMOTE"
git init -q -b main "$PUSH_REPO"
git -C "$PUSH_REPO" commit -q --allow-empty -m "base"
git -C "$PUSH_REPO" remote add origin "$PUSH_REMOTE"
git -C "$PUSH_REPO" push -q origin main
git -C "$PUSH_REPO" fetch -q origin
git -C "$PUSH_REPO" remote set-head origin --auto >/dev/null 2>&1 || true
git -C "$PUSH_REPO" checkout -q -b spira/sp-push1
printf 'push-work\n' > "$PUSH_REPO/push.txt"
git -C "$PUSH_REPO" add push.txt
git -C "$PUSH_REPO" commit -q -m "feat: sp-push1 — work"
git -C "$PUSH_REPO" checkout -q main

git init -q --bare -b main "$PR_REMOTE"
git init -q -b main "$PR_REPO"
git -C "$PR_REPO" commit -q --allow-empty -m "base"
git -C "$PR_REPO" remote add origin "$PR_REMOTE"
git -C "$PR_REPO" push -q origin main
git -C "$PR_REPO" fetch -q origin
git -C "$PR_REPO" remote set-head origin --auto >/dev/null 2>&1 || true
git -C "$PR_REPO" checkout -q -b spira/sp-pr1
printf 'pr-work\n' > "$PR_REPO/pr.txt"
git -C "$PR_REPO" add pr.txt
git -C "$PR_REPO" commit -q -m "feat: sp-pr1 — work"
git -C "$PR_REPO" checkout -q main

testdb_seed <<JSONL
{"id":"sp-push1","title":"push bead","status":"closed","issue_type":"task","labels":["repo:push-repo"],"updated_at":"2026-01-01T00:00:00Z"}
{"id":"sp-pr1","title":"pr bead","status":"closed","issue_type":"task","labels":["repo:pr-repo"],"updated_at":"2026-01-01T00:00:00Z"}
JSONL

cat > "$SH/repo-map" <<EOF
push-repo | $PUSH_REPO | push | origin/main | default | $SH/gate.sh
pr-repo   | $PR_REPO   | pr   | origin/main | default | $SH/gate.sh
EOF

stub gh 'exit 1'
rm -f "$RUN/gate-calls"

SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_REPO="$PUSH_REPO" \
SPIRA_HOME_REPO="push-repo" SPIRA_ID_PREFIX=sp \
SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
    bash "$SH/landing.sh" 2>/dev/null >/dev/null || true

want   "gate called for push-mode repo (positive control)" "push-repo" \
    "$(cat "$RUN/gate-calls" 2>/dev/null)"
nowant "gate not called for pr-mode repo" "pr-repo" \
    "$(cat "$RUN/gate-calls" 2>/dev/null)"

# ──────────────────────────────────────────────────────────────────────────────
# TEST 2: land_pr opens PR against master when base is origin/master
# ──────────────────────────────────────────────────────────────────────────────
MASTER_REMOTE="$TMP/master-remote.git"; MASTER_REPO="$TMP/master-repo"

git init -q --bare -b master "$MASTER_REMOTE"
git init -q -b master "$MASTER_REPO"
git -C "$MASTER_REPO" commit -q --allow-empty -m "base"
git -C "$MASTER_REPO" remote add origin "$MASTER_REMOTE"
git -C "$MASTER_REPO" push -q origin master
git -C "$MASTER_REPO" fetch -q origin
git -C "$MASTER_REPO" checkout -q -b spira/sp-master1
printf 'master-work\n' > "$MASTER_REPO/master.txt"
git -C "$MASTER_REPO" add master.txt
git -C "$MASTER_REPO" commit -q -m "feat: sp-master1 — work"
git -C "$MASTER_REPO" push -q origin spira/sp-master1
git -C "$MASTER_REPO" checkout -q master

# Stub ghq via SPIRA_GH: records pr create args; no existing PR for this branch.
# Unquoted heredoc so $PR_ARGS expands to the real path at write time; \$@ etc. are
# literal variable references in the stub script.
PR_ARGS="$TMP/pr-args"; : > "$PR_ARGS"
cat > "$SH/gh-master" <<GHEOF
#!/usr/bin/env bash
printf '%s\n' "\$@" >> "$PR_ARGS"
case "\$1 \$2" in
    "pr view")   printf ''; exit 0 ;;
    "pr list")   printf ''; exit 0 ;;
    "pr create") echo "42"; exit 0 ;;
    "pr merge")  exit 0 ;;
    *) exit 0 ;;
esac
GHEOF
chmod +x "$SH/gh-master"

(
    export SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
           SPIRA_GH="$SH/gh-master" GH_TIMEOUT=30 SPIRA_ID_PREFIX=sp
    . "$SH/lib.sh"
    log()    { :; }
    bdq()    { :; }
    bdjson() { echo '[]'; }
    land_pr "$MASTER_REPO" spira/sp-master1 sp-master1 origin/master
) >/dev/null 2>&1 || true

want "land_pr --base master (not main)" "--base master" "$(cat "$PR_ARGS" 2>/dev/null)"

# ──────────────────────────────────────────────────────────────────────────────
# TEST 3: duplicate PR suppression
#
# Positive control: NO existing open PR → ghq pr create IS called.
# Dup suppression: an open PR already carries every commit on our branch →
#   ghq pr create is NOT called.
#
# The positive control is what makes the suppression test meaningful: without it,
# a bug that always suppressed pr create would also pass the suppression check.
# ──────────────────────────────────────────────────────────────────────────────
DUP_REMOTE="$TMP/dup-remote.git"; DUP_REPO="$TMP/dup-repo"

git init -q --bare -b main "$DUP_REMOTE"
git init -q -b main "$DUP_REPO"
git -C "$DUP_REPO" commit -q --allow-empty -m "base"
git -C "$DUP_REPO" remote add origin "$DUP_REMOTE"
git -C "$DUP_REPO" push -q origin main
git -C "$DUP_REPO" fetch -q origin

# sp-new: the branch we are trying to land
git -C "$DUP_REPO" checkout -q -b spira/sp-new
printf 'work\n' > "$DUP_REPO/work.txt"
git -C "$DUP_REPO" add work.txt
git -C "$DUP_REPO" commit -q -m "feat: sp-new — work"
git -C "$DUP_REPO" push -q origin spira/sp-new

# sp-older: an open-PR branch that already contains sp-new's commit (branched from it)
git -C "$DUP_REPO" checkout -q -b spira/sp-older spira/sp-new
printf 'more\n' >> "$DUP_REPO/work.txt"
git -C "$DUP_REPO" add work.txt
git -C "$DUP_REPO" commit -q -m "feat: sp-older — extra"
git -C "$DUP_REPO" push -q origin spira/sp-older
# Fetch so origin/spira/sp-older exists locally for merge-base checks
git -C "$DUP_REPO" fetch -q origin
git -C "$DUP_REPO" checkout -q main

CREATE_LOG="$TMP/pr-create"

# POSITIVE CONTROL: no existing open PRs → ghq pr create IS called.
: > "$CREATE_LOG"
cat > "$SH/gh-noDup" <<GHEOF
#!/usr/bin/env bash
case "\$1 \$2" in
    "pr view")   printf ''; exit 0 ;;
    "pr list")   printf ''; exit 0 ;;
    "pr create") echo "created" >> "$CREATE_LOG"; echo "42"; exit 0 ;;
    "pr merge")  exit 0 ;;
    *) exit 0 ;;
esac
GHEOF
chmod +x "$SH/gh-noDup"

(
    export SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
           SPIRA_GH="$SH/gh-noDup" GH_TIMEOUT=30 SPIRA_ID_PREFIX=sp
    . "$SH/lib.sh"
    log()    { :; }
    bdq()    { :; }
    bdjson() { echo '[]'; }
    land_pr "$DUP_REPO" spira/sp-new sp-new origin/main
) >/dev/null 2>&1 || true
is "positive control: pr create called when no dup exists" "created" \
    "$(cat "$CREATE_LOG" 2>/dev/null)"

# DUP SUPPRESSION: sp-older is open and already carries sp-new's commit.
# The gh stub outputs the number-headRefName pair in the format land_pr's pipe expects:
#   "41 spira/sp-older"  (what `gh pr list -q '.[] | "\(.number) \(.headRefName)"'` produces)
: > "$CREATE_LOG"
cat > "$SH/gh-hasDup" <<GHEOF
#!/usr/bin/env bash
case "\$1 \$2" in
    "pr view")   printf ''; exit 0 ;;
    "pr list")   printf '41 spira/sp-older\n'; exit 0 ;;
    "pr create") echo "created" >> "$CREATE_LOG"; echo "42"; exit 0 ;;
    "pr merge")  exit 0 ;;
    *) exit 0 ;;
esac
GHEOF
chmod +x "$SH/gh-hasDup"

(
    export SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
           SPIRA_GH="$SH/gh-hasDup" GH_TIMEOUT=30 SPIRA_ID_PREFIX=sp
    . "$SH/lib.sh"
    log()    { :; }
    bdq()    { :; }
    bdjson() { echo '[]'; }
    land_pr "$DUP_REPO" spira/sp-new sp-new origin/main
) >/dev/null 2>&1 || true
is "dup suppression: pr create not called when dup exists" "" \
    "$(cat "$CREATE_LOG" 2>/dev/null)"

echo "test-landing-pass.sh"
[ "$fail" -eq 0 ] && printf 'ok %s\n' "$pass" || printf 'fail %s\n' "$fail"
[ "$fail" -eq 0 ]
