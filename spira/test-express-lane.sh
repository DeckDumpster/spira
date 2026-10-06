#!/usr/bin/env bash
# test-express-lane.sh — express label and sentinel admission bypass.
#
# tier: T3
# covers: spira/bead.sh spira/conf.sh cockpit-collect/src/* sentinel/src/* spira/lib.sh watchtower/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

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
tl_config SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD}"
REPONAME=fixture-repo
mkdir -p "$RUN/worktree" "$SH"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
cp -r "$HERE/chamber" "$SH/"
# SPIRA_CHAMBER no longer derives from SPIRA_HOME (the fixture declares its own path) —
# point it at this suite's own fixture chamber explicitly.
tl_config SPIRA_CHAMBER="$SH/chamber"

# Repo-map must exist before any bead.sh call; bdq validates repo: labels against it.
cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP
# SPIRA_REPO_MAP is a registered key too — no process reads env for it, and it does not
# derive from SPIRA_HOME any more either (same gap as SPIRA_CHAMBER above).
tl_config SPIRA_REPO_MAP="$SH/repo-map"

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
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED

echo "test-express-lane.sh"

# ======================================================================================
echo
echo "bead.sh file --express — express label on filed bead"
# ======================================================================================
# POSITIVE CONTROL: filing without --express produces no express label.
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
out="$(SPIRA_HOME="$SH" \
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
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
out="$(SPIRA_HOME="$SH" \
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
echo "P0 does not imply express — priority is not a designation"
# ======================================================================================
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
out="$(SPIRA_HOME="$SH" \
    bash "$SH/bead.sh" file "P0 blocker" \
    --for builder --repo fixture-repo --priority 0 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    LABELS="$(labels_of "$BID")"
    nowant "P0 alone: no express label" "express" "$LABELS"
else
    bad "P0 alone: could not create bead" "output: $out"
fi

# Pair: P0 + --express still gets the label, explicitly.
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
out="$(SPIRA_HOME="$SH" \
    bash "$SH/bead.sh" file "P0 blocker, express" \
    --for builder --repo fixture-repo --priority 0 --express 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    LABELS="$(labels_of "$BID")"
    want "P0 --express: express label present" "express" "$LABELS"
else
    bad "P0 --express: could not create bead" "output: $out"
fi

# Pair: P1 does NOT get express automatically.
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
out="$(SPIRA_HOME="$SH" \
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
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
out="$(SPIRA_HOME="$SH" \
    bash "$SH/bead.sh" file "amend target" \
    --for builder --repo fixture-repo --priority 2 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    # POSITIVE CONTROL: no express label before amend.
    LABELS="$(labels_of "$BID")"
    nowant "amend pre: no express label yet" "express" "$LABELS"
    # Amend with --express.
    SPIRA_HOME="$SH" \
        bash "$SH/bead.sh" amend "$BID" --express 2>&1 >/dev/null || true
    LABELS="$(labels_of "$BID")"
    want "amend --express: express label added" "express" "$LABELS"
else
    bad "amend --express: could not create bead" "output: $out"
fi

# ======================================================================================
echo
echo "sentinel CHECK7 express bypass — a throttled pass still summons an express bead"
# ======================================================================================
# sp-yh7yx: lib.sh's `express_ready_in_task_pool` (fbddd3e2b) was deleted outright at wave
# 4.25/sp-obhv6 as "no live callers" while `_ck7_summon_body`'s bash original still called
# it by name — the call failed silently every throttled pass from then on (an undefined
# bash function is "command not found", invisible in any diff), so "express ready" read
# false forever and the bypass never fired again. Wave 4.27/sp-gzmd2 ported
# `_ck7_summon_body` faithfully, carrying that silent `false` into
# `sentinel/src/summon.rs` with it. The pure throttle-leak arithmetic
# (`check7_pool_decision`'s own cases) is still `summon::tests::
# check7_pool_decision_matches_the_throttle_leak_fix`; THIS row is the missing piece —
# whether the pass actually asks the question, against the REAL builder fayth and a REAL
# bd, not a stub.
command -v aeon >/dev/null 2>&1 \
    || { echo "test-express-lane: aeon is not on PATH" >&2; exit 1; }
command -v sentinel >/dev/null 2>&1 \
    || { echo "test-express-lane: sentinel is not on PATH" >&2; exit 1; }

SUMMONED="$RUN/summoned.log"
cat > "$SH/mock-summon" <<'MOCK'
#!/usr/bin/env bash
fayth="${@: -1}"
printf 'SUMMONED:%s\n' "$fayth" >> "${SUMMONED_FILE:?}"
exit 0
MOCK
chmod +x "$SH/mock-summon"
# The pass's ready set is spira-claim's, over the lifecycle machine's READY rows (sp-v62vn:
# the only mode); the stand-in (testlib lc_mirror_bd) answers spira-lc `list` from this
# REAL bd store — an open bead is a READY row — ahead of the tree's spira-lc on PATH.
lc_mirror_bd "$TMP/lc"
sentinel_run() {
    # SPIRA_DB/SPIRA_BD ALSO AS PLAIN ENV: lc_mirror_bd's spira-lc stub (on PATH ahead of
    # the real one) is exec'd as sentinel's own child for its ready reads and reads them as
    # raw shell variables, never through spira-config — tl_config's declaration never reaches it.
    PATH="$TMP/lc:$PATH" SPIRA_HOME="$SH" \
        SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD}" \
        SPIRA_SUMMON="$SH/mock-summon" SUMMONED_FILE="$SUMMONED" SPIRA_CONF=/nonexistent \
        sentinel --summon-pass
}

# POSITIVE CONTROL: throttle stamp present, no express bead anywhere -> held at 0.
testdb_reset
testdb_seed <<'SEED'
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
SEED
rm -f "$SUMMONED" "$RUN/world.halted"
printf 'since=2026-09-21T00:00:00Z depth=20 since_land=60m\n' > "$RUN/queue-throttled"
out="$(sentinel_run 2>&1)" || true
want "no express bead ready: log says task pool held at 0" "task pool held at 0" "$out"
is   "no express bead ready: nothing summoned" \
     "absent" "$( [ -s "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

# THE BYPASS: file a builder bead with --express. Same throttle stamp, same empty pool —
# now summons despite it.
out="$(SPIRA_HOME="$SH" \
    bash "$SH/bead.sh" file "express bypass target" \
    --for builder --repo fixture-repo --priority 1 --express 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    rm -f "$SUMMONED"
    out="$(sentinel_run 2>&1)" || true
    want "express bead ready: bypass grants pool=1, restricted to 'express'" \
         "granting pool=1 (restricted to 'express')" "$out"
    want "express bead ready: builder is summoned despite the throttle" \
         "SUMMONED:builder" "$(cat "$SUMMONED" 2>/dev/null)"
else
    bad "express bypass: could not file the express bead" "output: $out"
fi

# ABSENCE: the same express bead, but the world is halted — the bypass still computes
# (pool math runs once per pass, before any per-persona attempt) but must never override
# a gate that sits above it in summon_fayth.
printf 'halted\n' > "$RUN/world.halted"
rm -f "$SUMMONED"
out="$(sentinel_run 2>&1)" || true
want "express ready but world halted: refused at the gate" "halted — not summoning" "$out"
is   "express ready but world halted: nothing summoned" \
     "absent" "$( [ -s "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

rm -f "$RUN/world.halted" "$RUN/queue-throttled" "$SUMMONED"

tl_summary
