#!/usr/bin/env bash
# test-express-lane.sh — express lane: label, admission bypass, cert order, batch trigger.
#
# THREE FIXTURE PAIRS (acceptance criteria from sp-m6jhx):
#   1. Sentinel CHECK7: throttle stamp present + express bead ready → 1 slot granted.
#      Pair: throttle stamp + no express bead → pool 0.
#   2. Landing certification: express branch among N is certified before the rest.
#      Pair: non-express branches keep refname order (express reorder is label-driven).
#   3. Batch: lone express certified branch triggers a batch before BATCH_MAX.
#      Pair: lone non-express certified branch waits (no trigger).
#
# PLUS: bead.sh file --express and bead.sh amend --express add the express label;
# P0 beads receive it automatically.
#
# covers: spira/sentinel.sh spira/landing.sh spira/batch.sh spira/bead.sh spira/lib.sh spira/conf.sh spira/watchtower.sh spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-express-lane
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up express-lane || {
    printf 'SKIP test-express-lane: server testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"; QUEUEDIR="$RUN/queue"
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

cp "$HERE"/*.sh "$SH/"
cp -r "$HERE/chamber" "$SH/"

# Repo-map must exist before any bead.sh call; bdq validates repo: labels against it.
cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
GATE_COUNT="$TMP/gate-count"; : > "$GATE_COUNT"
stub gate.sh '
printf "%s\n" "$1" >> "'"$GATE_COUNT"'"
printf "gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n" "$1" "${2:-?}" >&2
exit 0'
stub gh 'exit 1'

# Forge fixture: log pr-create calls, return incrementing PR numbers.
FORGE_LOG="$TMP/forge-log"; : > "$FORGE_LOG"
PR_COUNTER="$TMP/pr-counter"; printf '1\n' > "$PR_COUNTER"
cat > "$SH/forge-fixture.sh" << FORGE_EOF
#!/usr/bin/env bash
cmd="\${1:-}"; shift; shift  # skip repo arg
case "\$cmd" in
    pr-create)
        n=\$(cat "$PR_COUNTER"); echo \$(( n+1 )) > "$PR_COUNTER"
        printf '%s\n' "\$*" >> "$FORGE_LOG"
        printf 'pr=%s\n' "\$n"; exit 0 ;;
    *) exit 0 ;;
esac
FORGE_EOF
chmod +x "$SH/forge-fixture.sh"

stub repro.sh 'exit 0'

B() { bd -C "$SPIRA_DB" "$@"; }
labels_of() {
    B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []) if d else "")'
}
status_of() {
    B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "" if d else "")'
}

testdb_seed <<'SEED'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED

echo "test-express-lane.sh"

# ======================================================================================
echo
echo "bead.sh file --express — express label on filed bead"
# ======================================================================================
# POSITIVE CONTROL: filing without --express produces no express label.
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD}" \
    bash "$SH/bead.sh" file "express test bead" \
    --for builder --repo fixture-repo --priority 1 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    LABELS="$(labels_of "$BID")"
    nowant "file without --express: no express label" "express" "$LABELS"
fi

# FILING WITH --express: label must appear.
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD}" \
    bash "$SH/bead.sh" file "express test bead" \
    --for builder --repo fixture-repo --priority 1 --express 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    LABELS="$(labels_of "$BID")"
    want "file --express: express label present" "express" "$LABELS"
else
    bad "file --express: could not create bead" "output: $out"
fi

# ======================================================================================
echo
echo "P0 auto-express — P0 bead automatically gets the express label"
# ======================================================================================
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD}" \
    bash "$SH/bead.sh" file "P0 blocker" \
    --for builder --repo fixture-repo --priority 0 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    LABELS="$(labels_of "$BID")"
    want "P0 auto-express: express label present" "express" "$LABELS"
else
    bad "P0 auto-express: could not create bead" "output: $out"
fi

# Pair: P1 does NOT get express automatically.
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD}" \
    bash "$SH/bead.sh" file "P1 no express" \
    --for builder --repo fixture-repo --priority 1 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    LABELS="$(labels_of "$BID")"
    nowant "P1 no auto-express: no express label" "express" "$LABELS"
fi

# ======================================================================================
echo
echo "bead.sh amend --express — express label added to existing bead"
# ======================================================================================
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD}" \
    bash "$SH/bead.sh" file "amend target" \
    --for builder --repo fixture-repo --priority 2 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    # POSITIVE CONTROL: no express label before amend.
    LABELS="$(labels_of "$BID")"
    nowant "amend pre: no express label yet" "express" "$LABELS"
    # Amend with --express.
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD}" \
        bash "$SH/bead.sh" amend "$BID" --express 2>&1 >/dev/null || true
    LABELS="$(labels_of "$BID")"
    want "amend --express: express label added" "express" "$LABELS"
else
    bad "amend --express: could not create bead" "output: $out"
fi

# ======================================================================================
echo
echo "sentinel CHECK7 express bypass — express_ready_in_task_pool"
# ======================================================================================
# Use a mock fayth and a mock ready_count (no testdb needed for this pair).
# The function is in lib.sh; we override ready_count after sourcing.
T_FAYTH="$TMP/chamber"; mkdir -p "$T_FAYTH"
cat > "$T_FAYTH/builder.fayth" <<'FAYTH'
FAYTH_LABELS="spira,plan"
FAYTH_EXCLUDE_LABELS=""
FAYTH
export SPIRA_HOME="$TMP"
export SPIRA_RUN="$RUN"
export SPIRA_CONF="$TMP/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

# Override ready_count: returns non-zero when express label is in the labels arg.
EXPRESS_MATCH=""
ready_count() {
    case "$1" in *",express"*) printf '1' ;;
                 *) printf '0' ;; esac
}
# Pair 1 POSITIVE CONTROL (law-absence-needs-a-positive-control):
# Throttle stamp present + express bead ready → function returns 0.
THROTTLE_STAMP="$RUN/queue-throttled"
printf 'since=2026-09-21T00:00:00Z depth=20 since_land=60m\n' > "$THROTTLE_STAMP"
express_ready_in_task_pool "builder" "express" \
    && ok "throttle+express: express_ready_in_task_pool returns 0 (express ready)" \
    || bad "throttle+express: express_ready_in_task_pool returns 0 (express ready)" "returned 1"

# Pair: no express bead → function returns 1.
express_ready_in_task_pool "builder" "no-such-label" \
    && bad "throttle+no-express: express_ready_in_task_pool returns 1 (not ready)" "returned 0" \
    || ok "throttle+no-express: express_ready_in_task_pool returns 1 (not ready)"
rm -f "$THROTTLE_STAMP"

# ======================================================================================
echo
echo "landing: express branch certified before non-express (label-driven order)"
# ======================================================================================
# sp-b-express has express label and sorts after sp-a-regular alphabetically.
# After the express-first reorder, sp-b-express must be certified first.
export SPIRA_HOME="$SH"
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED

plant_bead_with_labels() {  # plant_bead_with_labels <id> <labels-json>
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":%s,"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$1" "$1" "$2" "$1" | testdb_seed
}

plant_bead_with_labels "sp-a-regular" '["spira","plan","repo:fixture-repo"]'
plant_bead_with_labels "sp-b-express" '["spira","plan","repo:fixture-repo","express"]'

wt_reg="$RUN/worktree/sp-a-regular"
git -C "$REPO" worktree add -q -b "spira/sp-a-regular" "$wt_reg" main
printf 'regular\n' > "$wt_reg/sp-a-regular.txt"
git -C "$wt_reg" add -A
git -C "$wt_reg" commit -q -m "sp-a-regular: work"

wt_exp="$RUN/worktree/sp-b-express"
git -C "$REPO" worktree add -q -b "spira/sp-b-express" "$wt_exp" main
printf 'express\n' > "$wt_exp/sp-b-express.txt"
git -C "$wt_exp" add -A
git -C "$wt_exp" commit -q -m "sp-b-express: work"

landing_run() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" \
        bash "$SH/landing.sh" 2>&1
}

out="$(landing_run)"
# The express-first log line must mention sp-b-express.
want "landing: express-first log reports express branch" \
    "express branch" "$out"
want "landing: express-first log names sp-b-express" \
    "spira/sp-b-express" "$out"

# sp-b-express must be certified before sp-a-regular in the output.
pos_express="$(printf '%s' "$out" | grep -n "certified spira/sp-b-express" | cut -d: -f1 | head -1)"
pos_regular="$(printf '%s' "$out" | grep -n "certified spira/sp-a-regular" | cut -d: -f1 | head -1)"
if [ -n "$pos_express" ] && [ -n "$pos_regular" ]; then
    [ "$pos_express" -lt "$pos_regular" ] \
        && ok "landing: express branch certified before non-express (lines $pos_express < $pos_regular)" \
        || bad "landing: express branch certified before non-express" \
               "express line $pos_express, regular line $pos_regular"
else
    bad "landing: could not find both certification lines in output" \
        "express=$pos_express regular=$pos_regular"
fi

# Pair: without express label, alphabetical order is preserved.
# sp-a-regular was certified before sp-b-express alphabetically (both now CERTIFIED).
# Clean for next case.
rm -f "$LANDSTATE/sp-a-regular" "$LANDSTATE/sp-b-express"
git -C "$REPO" branch -D spira/sp-a-regular spira/sp-b-express 2>/dev/null || true
git -C "$REPO" worktree prune 2>/dev/null || true

# ======================================================================================
echo
echo "batch: express certified branch triggers immediate batch (before BATCH_MAX)"
# ======================================================================================
# POSITIVE CONTROL: 1 express certified branch triggers a batch.
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED

plant_bead_with_labels "sp-expr-b" '["spira","plan","repo:fixture-repo","express"]'
plant_bead_with_labels "sp-norm-b" '["spira","plan","repo:fixture-repo"]'

# Create express branch + CERTIFIED landstate.
wt_e="$RUN/worktree/sp-expr-b"
git -C "$REPO" worktree add -q -b "spira/sp-expr-b" "$wt_e" main
printf 'x\n' > "$wt_e/sp-expr-b.txt"
git -C "$wt_e" add -A; git -C "$wt_e" commit -q -m "sp-expr-b: work"
tip_e="$(git -C "$REPO" rev-parse spira/sp-expr-b)"
NOW="$(date +%s)"
printf 'CERTIFIED %s %s\n' "$tip_e" "$NOW" > "$LANDSTATE/sp-expr-b"

# Create non-express branch + CERTIFIED landstate.
wt_n="$RUN/worktree/sp-norm-b"
git -C "$REPO" worktree add -q -b "spira/sp-norm-b" "$wt_n" main
printf 'y\n' > "$wt_n/sp-norm-b.txt"
git -C "$wt_n" add -A; git -C "$wt_n" commit -q -m "sp-norm-b: work"
tip_n="$(git -C "$REPO" rev-parse spira/sp-norm-b)"
printf 'CERTIFIED %s %s\n' "$tip_n" "$NOW" > "$LANDSTATE/sp-norm-b"

# Also seed a LANDED record so the stuck-queue age check has something to read.
printf 'LANDED fakeshafakeshafakeshafakeshafakeshafake %s\n' "$(( NOW - 60 ))" > "$LANDSTATE/sp-landed-anchor"

batch_run() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${TESTDB_BD}" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_BATCH_MAX=8 \
    SPIRA_QUEUE_BATCH_WAIT=9999 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_QUEUE_REPRO_BATCH="$SH/repro.sh" \
    SPIRA_QUEUE_LOCAL_GATE="" \
        bash "$SH/batch.sh" "$REPONAME" 2>&1
}

# Remove the non-express branch for the express-only test.
rm -f "$LANDSTATE/sp-norm-b"
git -C "$REPO" branch -D spira/sp-norm-b 2>/dev/null || true
git -C "$REPO" worktree prune 2>/dev/null || true
rm -f "$QUEUEDIR/$REPONAME/open"

out="$(batch_run)"
want "batch: express triggers immediately" "express certified branch" "$out"
[ -f "$QUEUEDIR/$REPONAME/open" ] \
    && ok  "batch: batch opened for express branch" \
    || bad "batch: batch opened for express branch" "open file not created"

# Pair: lone non-express branch waits — no batch triggered.
rm -f "$LANDSTATE/sp-expr-b" "$QUEUEDIR/$REPONAME/open"
git -C "$REPO" branch -D spira/sp-expr-b 2>/dev/null || true
git -C "$REPO" worktree prune 2>/dev/null || true

# sp-norm-b branch and worktree are still in place; just restore the landstate.
printf 'CERTIFIED %s %s\n' "$tip_n" "$NOW" > "$LANDSTATE/sp-norm-b"

out2="$(batch_run)"
nowant "batch pair: non-express lone branch does not trigger early" "express certified" "$out2"
[ ! -f "$QUEUEDIR/$REPONAME/open" ] \
    && ok  "batch pair: no batch opened for lone non-express branch" \
    || bad "batch pair: no batch opened for lone non-express branch" "open file unexpectedly created"

# ======================================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
