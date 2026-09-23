#!/usr/bin/env bash
# test-express-lane.sh — express label: bead.sh --express and P0 auto-express.
#
# covers: spira/bead.sh spira/conf.sh spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
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
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
