#!/usr/bin/env bash
# test-express-lane.sh — the express designation bead.sh files and amends; admission is a sentinel unit test.
#
# tier: T3
# covers: spira/bead.sh spira/conf.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-express-lane
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
printf '## Build\n\nA fixture change in spira/x.\n' > "$TMP/build.md"
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
timeout 5 git -C "$REPO" push -q origin main
timeout 5 git -C "$REPO" fetch -q origin

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SH/"
cp -r "$HERE/runes" "$SH/"
# SPIRA_RUNES_DIR no longer derives from SPIRA_HOME (the fixture declares its own path) —
# point it at this suite's own fixture runes explicitly.
tl_config SPIRA_RUNES_DIR="$SH/runes"

# Repo-map must exist before any bead.sh call; bdq validates repo: labels against it.
cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP
# SPIRA_REPO_MAP is a registered key too — no process reads env for it, and it does not
# derive from SPIRA_HOME any more either (same gap as SPIRA_RUNES_DIR above).
tl_config SPIRA_REPO_MAP="$SH/repo-map"

stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'

mkdir -p "$TMP/lcx"
cat > "$TMP/lcx/spira-lc" <<'LCX'
#!/usr/bin/env bash
MIRROR="$(dirname "$0")/../lc"
case "${1:-}" in
    express) mkdir -p "$SPIRA_RUN/lc-express"; : > "$SPIRA_RUN/lc-express/${2:?}"; echo "$2" >> "$MIRROR/express" ;;
    unexpress) rm -f "$SPIRA_RUN/lc-express/${2:?}" ;;
    *) [ ! -x "$MIRROR/spira-lc" ] || exec "$MIRROR/spira-lc" "$@" ;;
esac
exit 0
LCX
chmod +x "$TMP/lcx/spira-lc"
export SPIRA_RUN SPIRA_LC_BIN="$TMP/lcx/spira-lc"

B() { timeout 5 bd -C "$SPIRA_DB" "$@"; }
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
    --for builder --repo fixture-repo --body-file "$TMP/build.md" --priority 1 2>&1)" || true
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
    --for builder --repo fixture-repo --body-file "$TMP/build.md" --priority 1 --express 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    LABELS="$(labels_of "$BID")"
    nowant "file --express: no express label, express is lifecycle state" "express" "$LABELS"
    is "file --express: the lifecycle row is marked express" "yes" "$([ -f "$RUN/lc-express/$BID" ] && echo yes || echo no)"
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
    --for builder --repo fixture-repo --body-file "$TMP/build.md" --priority 0 2>&1)" || true
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
    --for builder --repo fixture-repo --body-file "$TMP/build.md" --priority 0 --express 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    LABELS="$(labels_of "$BID")"
    nowant "P0 --express: no express label, express is lifecycle state" "express" "$LABELS"
    is "P0 --express: the lifecycle row is marked express" "yes" "$([ -f "$RUN/lc-express/$BID" ] && echo yes || echo no)"
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
    --for builder --repo fixture-repo --body-file "$TMP/build.md" --priority 1 2>&1)" || true
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
    --for builder --repo fixture-repo --body-file "$TMP/build.md" --priority 2 2>&1)" || true
BID="$(printf '%s' "$out" | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID" ]; then
    # POSITIVE CONTROL: no express label before amend.
    LABELS="$(labels_of "$BID")"
    nowant "amend pre: no express label yet" "express" "$LABELS"
    # Amend with --express.
    SPIRA_HOME="$SH" \
        bash "$SH/bead.sh" amend "$BID" --express 2>&1 >/dev/null || true
    LABELS="$(labels_of "$BID")"
    nowant "amend --express: no express label, express is lifecycle state" "express" "$LABELS"
    is "amend --express: the lifecycle row is marked express" "yes" "$([ -f "$RUN/lc-express/$BID" ] && echo yes || echo no)"
else
    bad "amend --express: could not create bead" "output: $out"
fi

tl_summary
