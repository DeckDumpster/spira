#!/usr/bin/env bash
#
# test-cockpit-queue-section.sh — QUEUE section replaces UNLND.
#
# Acceptance criteria (each seen red before the implementation):
#   (a) batch of 3 members + 2 certified → pane shows batch #N with 3 members and
#       next with 2, in batcher order
#   (b) commit body mentioning an id does not mark it landed
#   (c) an open bead that is batched still appears under its batch
#   (d) the next-list order equals batch.sh's selection order (suite-trans, prio, epoch)
#
# Also: closed bead with branch but no landstate is counted as SP_UNLANDED_N anomaly.
#
# covers: spira/cockpit.sh spira/batch.sh spira/lib.sh cockpit/health.sh
# scar: UNLND read closed beads and commit bodies, so batched open beads were invisible
#       and body mentions falsely marked beads as landed.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require cockpit-queue-section
testdb_up cockpit-queue-section || exit 1

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
[ -n "$REAL_BD" ] || { echo "SKIP cockpit-queue-section: no bd binary" >&2; exit 77; }
TESTDB_BD_PATH="$(command -v bd)"

REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" commit --allow-empty -m "init" -q

# Batch members: sp-b1, sp-b2, sp-b3 (closed). Also sp-bopen (OPEN status, BATCHED).
# Next: sp-c1 (P1, older epoch), sp-c2 (P2, newer epoch).
# Anomaly: sp-noq (closed, branch, no landstate).
# Body-mention test: a commit on main has body mentioning sp-c1 (not a landing subject).
for br in sp-b1 sp-b2 sp-b3 sp-bopen sp-c1 sp-c2 sp-noq; do
    git -C "$REPO" checkout -q -b "spira/$br" main
    git -C "$REPO" commit --allow-empty -m "$br: work" -q
done
git -C "$REPO" checkout -q main

# A commit whose body mentions sp-c1 but subject is not a landing form.
git -C "$REPO" commit --allow-empty -F - -q <<'EOF'
other work: fixes an unrelated issue

This commit mentions sp-c1 in the body but is not a landing commit.
EOF

MAP="$TMP/repo-map"
printf '# name | path | land | base | format | gate\nalpha | %s | queue | main | |\n' "$REPO" > "$MAP"

RUN="$TMP/run"
mkdir -p "$RUN"
SPIRA_SCOPE_LABEL=alpha
for b in sp-b1 sp-b2 sp-b3 sp-bopen sp-c1 sp-c2 sp-noq; do
    printf '{"type":"system","subtype":"init"}\n' > "$RUN/$b.log"
done

NOW_EPOCH="$(date +%s)"
EPOCH_C1=$(( NOW_EPOCH - 600 ))
EPOCH_C2=$(( NOW_EPOCH - 300 ))
AGO10="$(date -u -d '10 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-10M +%Y-%m-%dT%H:%M:%SZ)"

testdb_seed <<JSONL
{"id":"sp-b1","title":"batch member 1","status":"closed","priority":2,"closed_at":"$AGO10","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-b2","title":"batch member 2","status":"closed","priority":2,"closed_at":"$AGO10","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-b3","title":"batch member 3","status":"closed","priority":2,"closed_at":"$AGO10","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-bopen","title":"open batched bead","status":"open","priority":2,"labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-c1","title":"next P1 item","status":"closed","priority":1,"closed_at":"$AGO10","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-c2","title":"next P2 item","status":"closed","priority":2,"closed_at":"$AGO10","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-noq","title":"anomaly bead","status":"closed","priority":3,"closed_at":"$AGO10","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
JSONL

# Landstate: batch members BATCHED, certified CERTIFIED; sp-noq has no landstate.
mkdir -p "$RUN/landstate"
TIP_B1="$(git -C "$REPO" rev-parse spira/sp-b1)"
TIP_B2="$(git -C "$REPO" rev-parse spira/sp-b2)"
TIP_B3="$(git -C "$REPO" rev-parse spira/sp-b3)"
TIP_BOPEN="$(git -C "$REPO" rev-parse spira/sp-bopen)"
TIP_C1="$(git -C "$REPO" rev-parse spira/sp-c1)"
TIP_C2="$(git -C "$REPO" rev-parse spira/sp-c2)"

printf 'BATCHED %s %s\n' "$TIP_B1" "$NOW_EPOCH" > "$RUN/landstate/sp-b1"
printf 'BATCHED %s %s\n' "$TIP_B2" "$NOW_EPOCH" > "$RUN/landstate/sp-b2"
printf 'BATCHED %s %s\n' "$TIP_B3" "$NOW_EPOCH" > "$RUN/landstate/sp-b3"
printf 'BATCHED %s %s\n' "$TIP_BOPEN" "$NOW_EPOCH" > "$RUN/landstate/sp-bopen"
printf 'CERTIFIED %s %s\n' "$TIP_C1" "$EPOCH_C1" > "$RUN/landstate/sp-c1"
printf 'CERTIFIED %s %s\n' "$TIP_C2" "$EPOCH_C2" > "$RUN/landstate/sp-c2"

# Open batch file: PR 42, members sp-b1 sp-b2 sp-b3 sp-bopen.
OPENED_EPOCH=$(( NOW_EPOCH - 180 ))
BATCH_DIR="$TMP/queue/alpha"
mkdir -p "$BATCH_DIR"
cat > "$BATCH_DIR/open" <<BATCHEOF
pr=42
head=$(git -C "$REPO" rev-parse main)
base=$(git -C "$REPO" rev-parse main)
members=sp-b1:$TIP_B1 sp-b2:$TIP_B2 sp-b3:$TIP_B3 sp-bopen:$TIP_BOPEN
opened=$OPENED_EPOCH
branch=spira/queue/test
BATCHEOF

out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO=alpha SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_PATH="$BD_PATH" \
    SPIRA_QUEUE_DIR="$TMP/queue" \
    bash "$HERE/cockpit.sh" once 2>/dev/null)"

val() { printf '%s' "$out" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

echo "--- (a) batch of 3+1 members and 2 certified ---"

is "SP_QUEUE_BATCH_N is 4" "4" "$(val SP_QUEUE_BATCH_N)"
want "SP_QUEUE_BATCH0 contains sp-b1" "sp-b1" "$(val SP_QUEUE_BATCH0)"
want "SP_QUEUE_BATCH1 contains sp-b2" "sp-b2" "$(val SP_QUEUE_BATCH1)"
want "SP_QUEUE_BATCH2 contains sp-b3" "sp-b3" "$(val SP_QUEUE_BATCH2)"
want "SP_QUEUE_BATCH3 contains sp-bopen" "sp-bopen" "$(val SP_QUEUE_BATCH3)"

is "SP_QUEUE_NEXT_N is 2" "2" "$(val SP_QUEUE_NEXT_N)"
# sp-noq is excluded: it has no landstate, so it is not CERTIFIED

echo "--- (b) commit body mention does not mark landed ---"
# sp-c1 appears in the commit body but not as a landing subject; it must NOT be landed.
is "SP_LANDED is 0 (no landing subject on main)" "0" "$(val SP_LANDED)"

echo "--- (c) open bead that is batched appears in batch section ---"
want "sp-bopen appears in batch rows" "sp-bopen" "$(printf '%s' "$out" | grep '^SP_QUEUE_BATCH')"
nowant "sp-bopen not in next rows" "sp-bopen" "$(printf '%s' "$out" | grep '^SP_QUEUE_NEXT')"

echo "--- (d) next-list order: sp-c1 (P1 older) before sp-c2 (P2 newer) ---"
want "SP_QUEUE_NEXT0 contains sp-c1" "sp-c1" "$(val SP_QUEUE_NEXT0)"
want "SP_QUEUE_NEXT1 contains sp-c2" "sp-c2" "$(val SP_QUEUE_NEXT1)"

echo "--- anomaly: closed bead with branch and no landstate ---"
is "SP_UNLANDED_N is 1 (sp-noq has no landstate)" "1" "$(val SP_UNLANDED_N)"

echo "--- positive controls ---"
want "output has SP_QUEUE_BATCH_N" "SP_QUEUE_BATCH_N=" "$out"
want "output has SP_QUEUE_NEXT_N" "SP_QUEUE_NEXT_N=" "$out"
want "output has SP_QUEUE_BATCH_PR=42" "SP_QUEUE_BATCH_PR=42" "$out"

echo "--- health.sh pane ---"
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

want "pane renders QUEUE label" "QUEUE" "$pane"
want "pane shows batch #42" "batch #42" "$pane"
want "pane renders sp-b1" "sp-b1" "$pane"
want "pane renders sp-c1" "sp-c1" "$pane"
want "pane renders sp-bopen" "sp-bopen" "$pane"
nowant "pane does not render old UNLND label" "UNLND" "$pane"

echo
printf 'test-cockpit-queue-section: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
