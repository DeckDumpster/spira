#!/usr/bin/env bash
#
# test-cockpit-queue.sh — queue and quarantine state in cockpit collector and panel.
#
#   ./test-cockpit-queue.sh
#
# THREE SHAPES:
#   sp-q1  closed, branch exists, landstate CERTIFIED  → SP_PEND shows [queued]
#   sp-q2  closed, branch exists, landstate BATCHED    → SP_PEND shows [batch #42]
#   sp-q3  closed, branch exists, no landstate         → SP_PEND shows no tag
#
# Also verifies:
#   SP_QUEUE_DEPTH counts CERTIFIED entries
#   SP_QUEUE_BATCH_PR/SP_QUEUE_BATCH_AGE read the open batch file
#   SP_QUEUE_QUARANTINE_N counts quarantined suites in suite-state
#   Health panel renders queue summary line
#
# covers: spira/cockpit.sh cockpit/health.sh spira/conf.sh spira/suite-state.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require cockpit-queue
testdb_up cockpit-queue || exit 1

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
BASE_PATH="$PATH"
BD_PATH="${SPIRA_PATH:-}"
REAL_BD="$(PATH="$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" command -v bd)"
[ -n "$REAL_BD" ] || { echo "SKIP cockpit-queue: no bd binary" >&2; exit 77; }
TESTDB_BD_PATH="$(command -v bd)"

REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" commit --allow-empty -m "init" -q
for br in sp-q1 sp-q2 sp-q3; do
    git -C "$REPO" checkout -q -b "spira/$br" main
    git -C "$REPO" commit --allow-empty -m "$br work" -q
done
git -C "$REPO" checkout -q main

MAP="$TMP/repo-map"
printf '# name | path | land | base | format | gate\nalpha | %s | queue | main | |\n' "$REPO" > "$MAP"

RUN="$TMP/run"
mkdir -p "$RUN"
for b in sp-q1 sp-q2 sp-q3; do
    printf '{"type":"system","subtype":"init"}\n' > "$RUN/$b.log"
done

AGO5="$(date -u -d '5 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-5M +%Y-%m-%dT%H:%M:%SZ)"
AGO8="$(date -u -d '8 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-8M +%Y-%m-%dT%H:%M:%SZ)"
AGO10="$(date -u -d '10 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-10M +%Y-%m-%dT%H:%M:%SZ)"

testdb_seed <<JSONL
{"id":"sp-q1","title":"certified branch","status":"closed","priority":1,"closed_at":"$AGO10","labels":["spira","plan","repo:alpha"]}
{"id":"sp-q2","title":"batched branch","status":"closed","priority":1,"closed_at":"$AGO8","labels":["spira","plan","repo:alpha"]}
{"id":"sp-q3","title":"no landstate","status":"closed","priority":1,"closed_at":"$AGO5","labels":["spira","plan","repo:alpha"]}
JSONL

# Landstate files: sp-q1 CERTIFIED, sp-q2 BATCHED, sp-q3 absent.
mkdir -p "$RUN/landstate"
TIP_Q1="$(git -C "$REPO" rev-parse spira/sp-q1)"
TIP_Q2="$(git -C "$REPO" rev-parse spira/sp-q2)"
NOW_EPOCH="$(date +%s)"
printf 'CERTIFIED %s %s\n' "$TIP_Q1" "$NOW_EPOCH" > "$RUN/landstate/sp-q1"
printf 'BATCHED %s %s\n'   "$TIP_Q2" "$NOW_EPOCH" > "$RUN/landstate/sp-q2"

# Open batch file: PR 42, opened 3 minutes ago.
OPENED_EPOCH=$(( NOW_EPOCH - 180 ))
BATCH_DIR="$TMP/queue/alpha"
mkdir -p "$BATCH_DIR"
cat > "$BATCH_DIR/open" <<BATCHEOF
pr=42
head=$TIP_Q2
base=$(git -C "$REPO" rev-parse main)
members=sp-q2:$TIP_Q2
opened=$OPENED_EPOCH
branch=spira/queue/test
BATCHEOF

# Suite-state with one quarantined suite.
mkdir -p "$REPO/spira"
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | sp-abc | flaky dns\n' \
    > "$REPO/spira/suite-state-test"

out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO=alpha \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_PATH="$BD_PATH" \
    SPIRA_QUEUE_DIR="$TMP/queue" \
    SPIRA_SUITE_STATE="spira/suite-state-test" \
    bash "$HERE/cockpit.sh" once 2>/dev/null)"

val() { printf '%s' "$out" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

echo "queue keys:"

# SP_QUEUE_DEPTH counts CERTIFIED landstate entries.
is "SP_QUEUE_DEPTH is 1" "1" "$(val SP_QUEUE_DEPTH)"

# SP_QUEUE_BATCH_PR reads the batch file.
is "SP_QUEUE_BATCH_PR is 42" "42" "$(val SP_QUEUE_BATCH_PR)"

# SP_QUEUE_BATCH_AGE is set (batch is ~3m old).
_age="$(val SP_QUEUE_BATCH_AGE)"
[ -n "$_age" ] && [ "$_age" != "0" ] && [ "$_age" != "?" ] \
    && ok "SP_QUEUE_BATCH_AGE is non-zero" \
    || bad "SP_QUEUE_BATCH_AGE is non-zero" "got [$_age]"

# SP_QUEUE_QUARANTINE_N counts the quarantined suite.
is "SP_QUEUE_QUARANTINE_N is 1" "1" "$(val SP_QUEUE_QUARANTINE_N)"

echo "SP_PEND row queue tags:"

# SP_PEND rows carry queue state tags.
# sp-q1 (CERTIFIED, oldest) should be first (oldest closed_at).
# sp-q2 (BATCHED) second, sp-q3 (no landstate) third.
want "SP_PEND0 contains sp-q1" "sp-q1" "$(val SP_PEND0)"
want "SP_PEND0 shows [queued]" "[queued]" "$(val SP_PEND0)"

want "SP_PEND1 contains sp-q2" "sp-q2" "$(val SP_PEND1)"
want "SP_PEND1 shows batch #42" "[batch #42]" "$(val SP_PEND1)"

want "SP_PEND2 contains sp-q3" "sp-q3" "$(val SP_PEND2)"
nowant "SP_PEND2 shows no queue tag" "[queued]" "$(val SP_PEND2)"
nowant "SP_PEND2 shows no batch tag" "[batch" "$(val SP_PEND2)"

echo "positive controls:"
want "output contains SP_QUEUE_DEPTH" "SP_QUEUE_DEPTH=" "$out"
want "output contains SP_QUEUE_BATCH_PR" "SP_QUEUE_BATCH_PR=" "$out"
want "output contains SP_QUEUE_QUARANTINE_N" "SP_QUEUE_QUARANTINE_N=" "$out"

echo "health panel:"
PANE="$HERE/../cockpit/health.sh"
{
    printf '%s\n' "$out" | python3 -c '
import sys
for line in sys.stdin:
    line = line.rstrip("\n")
    if "=" not in line: continue
    k, _, v = line.partition("=")
    k = k.strip()
    if not k or not (k[0].isalpha() or k[0] == "_"): continue
    print("%s=%s" % (k, "\x27" + v.replace("\x27", "\x27\\\x27\x27") + "\x27"))
'
} > "$RUN/cockpit.env"

pane="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_FAYTHS=t \
    bash "$PANE" once 0 120 2>/dev/null)"

want "pane renders UNLND section" "UNLND" "$pane"
want "pane shows queue depth in summary" "depth 1" "$pane"
want "pane shows batch PR in summary" "batch #42" "$pane"
want "pane shows quarantined count in summary" "quarantined" "$pane"
want "pane renders sp-q1" "sp-q1" "$pane"
want "pane shows [queued] for sp-q1" "[queued]" "$pane"
want "pane renders sp-q2" "sp-q2" "$pane"

echo
printf 'test-cockpit-queue: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
