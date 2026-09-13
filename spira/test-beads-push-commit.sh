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
# WITHOUT committing, and asserts the statute is NOT visible via a fresh clone. Only
# then does it run beads-push.sh (which commits and pushes) and assert the statute IS
# visible via a fresh clone.
#
# If the commit step is removed from beads-push.sh, the positive control passes (0
# as expected) but the real test fails (clone still shows 0, expected 1) — so the
# suite goes red whenever the fix is absent.
#
# HOW CLONING IS VERIFIED (no dolt clone)
# ----------------------------------------
# bd-embedded dolt push writes to a file:// directory using the remote API format.
# The standalone dolt 2.2.3 in the gate container cannot clone that format: its
# `dolt clone` uses a client-side protocol implementation that diverges from what
# the embedded library wrote. Using `bd bootstrap` avoids this: bootstrap uses the
# same embedded library that wrote the remote, so the format is always compatible.
# After bootstrap, `bd memories --json` counts the target statute key.
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
testdb_require beads-push-commit
testdb_up beads-push-commit
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

# _count_in_remote <key> <url>
# How many memories with exactly the given key appear in a fresh bootstrap from <url>.
# Uses bd bootstrap (same embedded library as the pusher) rather than dolt clone to
# avoid the file:// format incompatibility between bd-embedded and standalone dolt.
_count_in_remote() {
    local key="$1" url="$2"
    local d; d="$(mktemp -d)"
    mkdir -p "$d/.beads"
    printf 'sync.remote: "%s"\n' "$url" > "$d/.beads/config.yaml"
    BD_NON_INTERACTIVE=1 bd -C "$d" bootstrap --yes >/dev/null 2>&1 \
        || { rm -rf "$d"; printf '?'; return; }
    local count
    count="$(bd -C "$d" memories --json 2>/dev/null \
        | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    k = sys.argv[1]
    print(sum(1 for key in d if key == k))
except Exception:
    print("?")
' "$key" 2>/dev/null)" || count="?"
    rm -rf "$d"
    printf '%s' "${count:-?}"
}

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
# WITHOUT committing. The fresh bootstrap clone must NOT see the statute.
echo
echo "positive control — push without commit does not include dirty statute:"

bd -C "$SPIRA_DB" --dolt-auto-commit off \
    remember --key "law-push-test" "Push test statute." >/dev/null 2>&1 || {
    bad "positive control: write statute" "bd remember failed"; exit 1; }

# Simulate old beads-push.sh: push directly, skipping the commit step.
bd -C "$SPIRA_DB" dolt push --remote beads >/dev/null 2>&1

count_pc="$(_count_in_remote "law-push-test" "file://$REMOTE")"
is "positive control: dirty statute absent from clone without commit" "0" "$count_pc"

# ── REAL TEST ─────────────────────────────────────────────────────────────────
# Run beads-push.sh (which now commits dirty tables). The statute must appear in
# a fresh bootstrap clone.
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

count_real="$(_count_in_remote "law-push-test" "file://$REMOTE")"
is "real test: statute present in clone after beads-push.sh" "1" "$count_real"

# ── NO-OP TEST ────────────────────────────────────────────────────────────────
# Run beads-push.sh again with nothing dirty. It must not create a new commit.
# The Dolt manifest file records the current commit hash; a new commit updates it.
echo
echo "no-op test — beads-push.sh with clean working set makes no commit:"

EMBDIR="$SPIRA_DB/.beads/embeddeddolt"
DBNAME="$(ls "$EMBDIR" 2>/dev/null | grep -v '^\.' | grep -v '^\.lock$' | head -1)"
MANIFEST="$EMBDIR/$DBNAME/.dolt/noms/manifest"

manifest_before="$(cat "$MANIFEST" 2>/dev/null)"
SPIRA_DB="$SPIRA_DB" bash "$ROOT/beads-push.sh" >/dev/null 2>&1
manifest_after="$(cat "$MANIFEST" 2>/dev/null)"
is "no-op: clean state produces no new commit" "$manifest_before" "$manifest_after"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
