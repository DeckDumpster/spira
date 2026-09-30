#!/usr/bin/env bash
#
# test-cockpit-queue-section.sh — QUEUE section replaces UNLND.
#
# Acceptance criteria (each seen red before the implementation):
#   (a) batch of 4 members + 2 certified → pane shows batch #N with 4 members and
#       next with 2, in batcher order
#   (c) an open bead that is batched still appears under its batch
#   (d) the next-list order equals the batcher's selection order (suite-trans, prio, epoch)
#   (e) 32 CERTIFIED records, no open batch → header count is the whole 32, row count
#       is capped (sp-hpeft: the header was reading the row-capped loop counter, so it
#       could never show more than the cap regardless of how many records existed)
#
# ABSORBED FROM test-cockpit-queue.sh (coverage row 11 / cluster 2): QUARANTINE_N (count of
# quarantined suites in suite-state) and a non-zero BATCH_AGE. Its own copies of the batch/
# next/pane assertions were identical to this suite's and are not repeated (test-cockpit-
# queue.sh is deleted).
#
# The anomaly row (closed bead, branch, no landstate) and the commit-body-mention case (b)
# both moved to test-cockpit-unlanded.sh (coverage rows 10/11, cluster 3): both concern
# unsent_keys' SP_LANDED/SP_UNLANDED_N classification, not queue_keys, and cannot be
# exercised through the `queue` subcommand at all once this suite stopped calling `once`.
#
# covers: cockpit-collect/src/* spira/lib.sh cockpit/health.sh
# scar: UNLND read closed beads and commit bodies, so batched open beads were invisible
#       and body mentions falsely marked beads as landed.
#
# SPIRA_BDJSON_FIXTURE, NOT A REAL STORE. queue_keys' only bd reads that this suite exercises
# are two `show <ids>` calls (batch members, next/certified candidates); the query shape
# itself is covered once in test-cockpit-bd-contract.sh, against real bd. queue_keys also
# issues a `bd ready --label express` read for SP_EXPRESS_N — bdsim.py deliberately does not
# simulate `ready` (a hand-written model of its blocker-aware filtering would drift from the
# real one), so under the fixture that read fails and SP_EXPRESS_N renders 0. Neither this
# suite nor its predecessor asserts SP_EXPRESS_N, so nothing here depends on that read
# succeeding; a suite that needs it belongs in test-cockpit-bd-contract.sh, against real bd.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
BASE_PATH="$PATH"

REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" commit --allow-empty -m "init" -q

# Batch members: sp-b1, sp-b2, sp-b3 (closed). Also sp-bopen (OPEN status, BATCHED).
# Next: sp-c1 (P1, older epoch), sp-c2 (P2, newer epoch).
for br in sp-b1 sp-b2 sp-b3 sp-bopen sp-c1 sp-c2; do
    git -C "$REPO" checkout -q -b "spira/$br" main
    git -C "$REPO" commit --allow-empty -m "$br: work" -q
done
git -C "$REPO" checkout -q main

MAP="$TMP/repo-map"
printf '# name | path | land | base | format | gate\nalpha | %s | queue | main | |\n' "$REPO" > "$MAP"

RUN="$TMP/run"
mkdir -p "$RUN"
SPIRA_SCOPE_LABEL=alpha

cat > "$TMP/beads.json" <<JSON
[
  {"id":"sp-b1","title":"batch member 1","status":"closed","priority":2,"labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]},
  {"id":"sp-b2","title":"batch member 2","status":"closed","priority":2,"labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]},
  {"id":"sp-b3","title":"batch member 3","status":"closed","priority":2,"labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]},
  {"id":"sp-bopen","title":"open batched bead","status":"open","priority":2,"labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]},
  {"id":"sp-c1","title":"next P1 item","status":"closed","priority":1,"labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]},
  {"id":"sp-c2","title":"next P2 item","status":"closed","priority":2,"labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
]
JSON

# Landstate: batch members BATCHED, certified CERTIFIED.
mkdir -p "$RUN/landstate"
NOW_EPOCH="$(date +%s)"
EPOCH_C1=$(( NOW_EPOCH - 600 ))
EPOCH_C2=$(( NOW_EPOCH - 300 ))
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

# Open batch file: PR 42, members sp-b1 sp-b2 sp-b3 sp-bopen, opened 180s ago (ABSORBED FROM
# test-cockpit-queue.sh: a non-zero SP_QUEUE_BATCH_AGE).
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

# ABSORBED FROM test-cockpit-queue.sh: one quarantined suite in the repo's suite-state file.
mkdir -p "$REPO/spira"
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | sp-abc | flaky dns\n' \
    > "$REPO/spira/suite-state-test"

# Run cockpit-collect probe queue — the probe's own subcommand, not a full `once` — with bd reads
# answered from a canned-JSON fixture rather than a live store.
queue() {
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
        SPIRA_REPO="$REPO" SPIRA_HOME_REPO=alpha SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$MAP" SPIRA_FAYTHS=t \
        SPIRA_QUEUE_DIR="$TMP/queue" \
        SPIRA_SUITE_STATE_FILE="spira/suite-state-test" \
        SPIRA_BDJSON_FIXTURE="$TMP/beads.json" \
        cockpit-collect probe queue 2>/dev/null
}

out="$(queue)"
val() { printf '%s' "$out" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

echo "--- (a) batch of 4 members and 2 certified ---"

is "SP_QUEUE_BATCH_N is 4" "4" "$(val SP_QUEUE_BATCH_N)"
want "SP_QUEUE_BATCH0 contains sp-b1" "sp-b1" "$(val SP_QUEUE_BATCH0)"
want "SP_QUEUE_BATCH1 contains sp-b2" "sp-b2" "$(val SP_QUEUE_BATCH1)"
want "SP_QUEUE_BATCH2 contains sp-b3" "sp-b3" "$(val SP_QUEUE_BATCH2)"
want "SP_QUEUE_BATCH3 contains sp-bopen" "sp-bopen" "$(val SP_QUEUE_BATCH3)"

is "SP_QUEUE_NEXT_N is 2" "2" "$(val SP_QUEUE_NEXT_N)"
is "SP_QUEUE_DEPTH is 2 (sp-c1, sp-c2 CERTIFIED)" "2" "$(val SP_QUEUE_DEPTH)"

echo "--- (c) open bead that is batched appears in batch section ---"
want "sp-bopen appears in batch rows" "sp-bopen" "$(printf '%s' "$out" | grep '^SP_QUEUE_BATCH')"
nowant "sp-bopen not in next rows" "sp-bopen" "$(printf '%s' "$out" | grep '^SP_QUEUE_NEXT')"

echo "--- (d) next-list order: sp-c1 (P1 older) before sp-c2 (P2 newer) ---"
want "SP_QUEUE_NEXT0 contains sp-c1" "sp-c1" "$(val SP_QUEUE_NEXT0)"
want "SP_QUEUE_NEXT1 contains sp-c2" "sp-c2" "$(val SP_QUEUE_NEXT1)"

echo "--- quarantine and batch age (absorbed from test-cockpit-queue.sh) ---"
is "SP_QUEUE_QUARANTINE_N is 1" "1" "$(val SP_QUEUE_QUARANTINE_N)"
_age="$(val SP_QUEUE_BATCH_AGE)"
[ -n "$_age" ] && [ "$_age" != "0" ] && [ "$_age" != "?" ] \
    && ok "SP_QUEUE_BATCH_AGE is non-zero" \
    || bad "SP_QUEUE_BATCH_AGE is non-zero" "got [$_age]"

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

REAL_BD="$(command -v "${SPIRA_BD:-bd}" 2>/dev/null)"
pane="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_BD="${REAL_BD:-bd}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_FAYTHS=t \
    bash "$PANE" once 0 120 2>/dev/null)"

want "pane renders QUEUE label" "QUEUE" "$pane"
want "pane shows batch #42" "batch #42" "$pane"
want "pane renders sp-b1" "sp-b1" "$pane"
want "pane renders sp-c1" "sp-c1" "$pane"
want "pane renders sp-bopen" "sp-bopen" "$pane"
nowant "pane does not render old UNLND label" "UNLND" "$pane"

echo "--- (e) 32 certified, no open batch: header is the whole count, rows are capped ---"
TMP2="$(mktemp -d)"
trap 'rm -rf "$TMP" "$TMP2"' EXIT INT TERM
REPO2="$TMP2/repo"
git init -q -b main "$REPO2"
git -C "$REPO2" commit --allow-empty -m "init" -q

N_CERT=32
BEADS2_JSON="$TMP2/beads.json"
{
    printf '[\n'
    for i in $(seq 1 "$N_CERT"); do
        bid="$(printf 'sp-e%02d' "$i")"
        git -C "$REPO2" checkout -q -b "spira/$bid" main
        git -C "$REPO2" commit --allow-empty -m "$bid: work" -q
        [ "$i" -gt 1 ] && printf ',\n'
        printf '  {"id":"%s","title":"cert item %d","status":"closed","priority":2,"labels":["%s","plan","repo:alpha"]}' \
            "$bid" "$i" "$SPIRA_SCOPE_LABEL"
    done
    printf '\n]\n'
} > "$BEADS2_JSON"
git -C "$REPO2" checkout -q main

RUN2="$TMP2/run"
mkdir -p "$RUN2/landstate" "$TMP2/queue"
NOW2="$(date +%s)"
for i in $(seq 1 "$N_CERT"); do
    bid="$(printf 'sp-e%02d' "$i")"
    tip="$(git -C "$REPO2" rev-parse "spira/$bid")"
    printf 'CERTIFIED %s %s\n' "$tip" "$(( NOW2 - i ))" > "$RUN2/landstate/$bid"
done

MAP2="$TMP2/repo-map"
printf '# name | path | land | base | format | gate\nalpha | %s | queue | main | |\n' "$REPO2" > "$MAP2"

out2="$(env -i PATH="$BASE_PATH" HOME="$TMP2" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP2/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$REPO2" SPIRA_HOME_REPO=alpha SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
    SPIRA_RUN="$RUN2" SPIRA_DB="$TMP2/nodb" \
    SPIRA_REPO_MAP="$MAP2" SPIRA_FAYTHS=t \
    SPIRA_QUEUE_DIR="$TMP2/queue" \
    SPIRA_BDJSON_FIXTURE="$BEADS2_JSON" \
    cockpit-collect probe queue 2>/dev/null)"
val2() { printf '%s' "$out2" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

is "SP_QUEUE_NEXT_N is the whole 32, not the row cap" "32" "$(val2 SP_QUEUE_NEXT_N)"

_rows2="$(printf '%s\n' "$out2" | grep -c '^SP_QUEUE_NEXT[0-9]')"
[ "$_rows2" -gt 0 ] && [ "$_rows2" -le 20 ] \
    && ok "SP_QUEUE_NEXT rows emitted are capped (got $_rows2)" \
    || bad "SP_QUEUE_NEXT rows emitted are capped" "got [$_rows2]"

PANE2="$HERE/../cockpit/health.sh"
{
    printf '%s\n' "$out2" | python3 -c '
import sys
for line in sys.stdin:
    line = line.rstrip("\n")
    if "=" not in line: continue
    k, _, v = line.partition("=")
    k = k.strip()
    if not k or not (k[0].isalpha() or k[0] == "_"): continue
    print("%s=%s" % (k, "\x27" + v.replace("\x27", "\x27\\\x27\x27") + "\x27"))
'
} > "$RUN2/cockpit.env"

pane2="$(env -i PATH="$BASE_PATH" HOME="$TMP2" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP2/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$REPO2" \
    SPIRA_RUN="$RUN2" SPIRA_DB="$TMP2/nodb" SPIRA_BD="${REAL_BD:-bd}" \
    SPIRA_REPO_MAP="$MAP2" SPIRA_FAYTHS=t \
    bash "$PANE2" once 0 120 2>/dev/null)"

want "pane header reads 32 certified" "32 certified" "$pane2"
nowant "pane header does not read 20 certified" "20 certified" "$pane2"
_prows2="$(printf '%s\n' "$pane2" | grep -c 'sp-e[0-9][0-9]')"
[ "$_prows2" -gt 0 ] && [ "$_prows2" -le 20 ] \
    && ok "pane rows are capped at the row cap (got $_prows2)" \
    || bad "pane rows are capped at the row cap" "got [$_prows2]"

echo
tl_summary
