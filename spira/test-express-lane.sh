#!/usr/bin/env bash
# test-express-lane.sh — express label and sentinel admission bypass.
#
# covers: spira/bead.sh spira/conf.sh spira/cockpit.sh spira/sentinel.sh spira/lib.sh spira/watchtower.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-express-lane
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up express-lane || {
    printf 'SKIP test-express-lane: testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
REPONAME=fixture-repo
mkdir -p "$RUN/worktree" "$SH"

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

B() { bd -C "$SPIRA_DB" "$@"; }
labels_of() {
    B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []) if d else "")'
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
# Override ready_count after sourcing lib.sh: returns non-zero when express label is
# in the labels arg (simulates a ready express bead without a full testdb query).
T_FAYTH="$TMP/chamber"; mkdir -p "$T_FAYTH"
cat > "$T_FAYTH/builder.fayth" <<'FAYTH'
FAYTH_LABELS="spira,plan"
FAYTH_EXCLUDE_LABELS=""
FAYTH
export SPIRA_HOME="$TMP"
export SPIRA_CONF="$TMP/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

ready_count() {
    case "$1" in *",express"*) printf '1' ;;
                 *) printf '0' ;; esac
}

# POSITIVE CONTROL: throttle stamp present + express bead ready → returns 0.
THROTTLE_STAMP="$RUN/queue-throttled"
printf 'since=2026-09-21T00:00:00Z depth=20 since_land=60m\n' > "$THROTTLE_STAMP"
express_ready_in_task_pool "builder" "express" \
    && ok "throttle+express: express_ready_in_task_pool returns 0 (express ready)" \
    || bad "throttle+express: express_ready_in_task_pool returns 0 (express ready)" "returned 1"

# Pair: no express bead → returns 1.
express_ready_in_task_pool "builder" "no-such-label" \
    && bad "throttle+no-express: express_ready_in_task_pool returns 1 (not ready)" "returned 0" \
    || ok "throttle+no-express: express_ready_in_task_pool returns 1 (not ready)"
rm -f "$THROTTLE_STAMP"

# Pair: express bead outside this fayth's partition does not grant a slot.
# (fayth file absent → express_ready_in_task_pool skips it and returns 1)
express_ready_in_task_pool "nonexistent-fayth" "express" \
    && bad "express outside partition: absent fayth grants no slot" "returned 0" \
    || ok "express outside partition: absent fayth grants no slot"

# ======================================================================================
echo
echo "check7_pool_decision — the throttle leak (sp-zcvh1)"
# ======================================================================================
# THE DEFECT. sentinel.sh CHECK 7 raised a throttled pool toward 1 but never capped it:
# `[ "${pool:-0}" -lt 1 ] && pool=1` left a free value already >= 1 (e.g. 5, the free slot
# count with no builders live) untouched, so one ready express bead switched the throttle
# off for the whole pass instead of admitting the one express bead it was meant for.
#
# throttled + 0 express beads → 0 (the ordinary hold).
is "throttled, no express: pool held at 0" "0" \
   "$(check7_pool_decision 1 5 0)"

# throttled + 1 express bead ready + 5 free → 1, NOT 5. This is the positive control for
# the fix: the old inline form returns free (5) here, which is the leak itself.
is "throttled, express ready, 5 free: pool granted exactly 1" "1" \
   "$(check7_pool_decision 1 5 1)"

# unthrottled → free, unchanged. The function must not touch a pool the throttle never
# engaged.
is "unthrottled: pool passes through as free" "5" \
   "$(check7_pool_decision 0 5 0)"
is "unthrottled: pool passes through as free even with an express bead ready" "5" \
   "$(check7_pool_decision 0 5 1)"

# ======================================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
