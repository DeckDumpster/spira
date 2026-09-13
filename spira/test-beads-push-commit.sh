#!/usr/bin/env bash
#
# test-beads-push-commit.sh — beads-push.sh commits dirty tracked tables before pushing.
#
# WHAT THIS TESTS
# ---------------
# bd never creates a Dolt commit for config writes (statutes, memories). Without an
# explicit commit before push, the statute book diverges from the remote and a fresh
# clone is short however many statutes were enacted since the last issue write.
#
# The fix: beads-push.sh calls bd dolt commit before every push; an empty working
# set is a no-op and never produces an empty commit.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# -------------------------------------------------------
# This suite would pass without the fix in embedded mode (where bd-embedded
# auto-commits every write). The positive control defeats that: it writes a statute
# with --dolt-auto-commit off to put the config table in a dirty state, then pushes
# WITHOUT committing, and asserts the statute is NOT in a fresh clone. Only then
# does it run beads-push.sh (which commits and pushes) and assert the statute IS
# in a fresh clone.
#
# If the commit step is removed from beads-push.sh, the positive control passes (0
# as expected) but the real test fails (clone still shows 0, expected 1) — so the
# suite goes red whenever the fix is absent.
#
# HOW THE FILE:// REMOTE WORKS
# ----------------------------
# bd-embedded dolt push writes to a directory using the remote API format, not the
# standard .dolt/ layout. The remote directory must be empty before the first push —
# a pre-initialised directory (dolt init) adds a .dolt/ subdirectory that confuses
# the manifest and produces an empty clone. dolt clone understands the remote API
# format when the directory has only the push-written files.
#
# defect: sp-e1l1
# covers: beads-push.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-beads-push-commit.sh"
TMP="$(mktemp -d)"

. "$HERE/testdb.sh"
testdb_up beads-push-commit
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

command -v dolt >/dev/null 2>&1 || { echo "SKIP: dolt not found on PATH"; exit 0; }

# ── SETUP ─────────────────────────────────────────────────────────────────────
echo
echo "setup — local remote + initial seed push:"

# Remote directory must be EMPTY before the first push (see comment above).
REMOTE="$TMP/remote"
mkdir -p "$REMOTE"

# Add a Dolt remote named "beads" — the name beads-push.sh hardcodes.
bd -C "$SPIRA_DB" dolt remote add beads "file://$REMOTE" 2>/dev/null || {
    bad "setup: add remote" "bd dolt remote add failed"; exit 1; }

# beads-push.sh gate: presence of sync.remote: in config.yaml.
printf '\nsync.remote: beads\n' >> "$SPIRA_DB/.beads/config.yaml"

# Seed the remote with the current state so future pushes are incremental.
bd -C "$SPIRA_DB" dolt push --remote beads >/dev/null 2>&1 || {
    bad "setup: seed push" "initial dolt push failed"; exit 1; }
ok "setup: seed push"

# ── POSITIVE CONTROL ──────────────────────────────────────────────────────────
# Write a statute in dirty mode (--dolt-auto-commit off simulates server-mode
# behaviour where bd writes config without creating a Dolt commit), then push
# WITHOUT committing. The clone must NOT contain the statute.
echo
echo "positive control — push without commit does not include dirty statute:"

bd -C "$SPIRA_DB" --dolt-auto-commit off \
    remember --key "law-push-test" "Push test statute." >/dev/null 2>&1 || {
    bad "positive control: write statute" "bd remember failed"; exit 1; }

# Simulate old beads-push.sh: push directly, skipping the commit step.
bd -C "$SPIRA_DB" dolt push --remote beads >/dev/null 2>&1

CLONE_PC="$TMP/clone-pc"
dolt clone "file://$REMOTE" "$CLONE_PC" >/dev/null 2>&1 || {
    bad "positive control: clone" "dolt clone failed"; exit 1; }

count_pc="$(cd "$CLONE_PC" && dolt sql -r csv \
    -q "SELECT COUNT(*) FROM config WHERE \`key\` = 'kv.memory.law-push-test'" \
    2>/dev/null | tail -1 | tr -d '\r')"
is "positive control: dirty statute absent from clone without commit" "0" "$count_pc"

# ── REAL TEST ─────────────────────────────────────────────────────────────────
# Run beads-push.sh (which now commits dirty tables). The statute must appear in
# a fresh clone.
echo
echo "real test — beads-push.sh commits dirty tables and pushes:"

# The statute written above is still dirty in the database; beads-push.sh must
# commit it, then push, then the clone must find it.
push_out="$(SPIRA_DB="$SPIRA_DB" bash "$ROOT/beads-push.sh" 2>&1)"
push_rc=$?
is "beads-push.sh exits 0" "0" "$push_rc"

# The pre-push commit line must mention the dirty table count.
if [[ "$push_out" == *"committed"*"dirty table"* ]]; then
    ok "real test: output mentions committed dirty tables"
else
    bad "real test: output mentions committed dirty tables" \
        "did not find 'committed ... dirty table' in: $push_out"
fi

CLONE_REAL="$TMP/clone-real"
dolt clone "file://$REMOTE" "$CLONE_REAL" >/dev/null 2>&1 || {
    bad "real test: clone" "dolt clone failed"; exit 1; }

count_real="$(cd "$CLONE_REAL" && dolt sql -r csv \
    -q "SELECT COUNT(*) FROM config WHERE \`key\` = 'kv.memory.law-push-test'" \
    2>/dev/null | tail -1 | tr -d '\r')"
is "real test: statute present in clone after beads-push.sh" "1" "$count_real"

# ── NO-OP TEST ────────────────────────────────────────────────────────────────
# Run beads-push.sh again with nothing dirty. It must not create a new commit.
echo
echo "no-op test — beads-push.sh with clean working set makes no commit:"

EMBDIR="$SPIRA_DB/.beads/embeddeddolt"
DBNAME="$(ls "$EMBDIR" 2>/dev/null | grep -v '^\.' | grep -v '^\.lock$' | head -1)"

log_before="$(cd "$EMBDIR/$DBNAME" && dolt log --oneline 2>/dev/null | head -1)"
SPIRA_DB="$SPIRA_DB" bash "$ROOT/beads-push.sh" >/dev/null 2>&1
log_after="$(cd "$EMBDIR/$DBNAME" && dolt log --oneline 2>/dev/null | head -1)"
is "no-op: clean state produces no new commit" "$log_before" "$log_after"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
