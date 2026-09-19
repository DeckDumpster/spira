#!/usr/bin/env bash
# test-queue-abandon.sh — queue.sh abandon closes the batch PR, re-certifies innocent
# members, leaves RED/EJECTED members alone, archives the open record.
#
# covers: spira/queue.sh spira/forge.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-queue-abandon.sh"

. "$HERE/testdb.sh"
testdb_require test-queue-abandon
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up queue-abandon || { echo "test-queue-abandon: could not build fixture database"; exit 1; }

REPO="$TMP/repo"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixq
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"
FORGE_LOG="$TMP/forge.log"

git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m init
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"
cp "$HERE"/*.sh "$SH/"

# Fake forge: records arguments, succeeds silently.
cat > "$SH/forge-fake.sh" <<ENDFAKE
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "\$FORGE_LOG"
exit 0
ENDFAKE
chmod +x "$SH/forge-fake.sh"

RMAP="$TMP/repo-map"
printf '%s | %s | queue | main | | |\n' "$REPONAME" "$REPO" > "$RMAP"

run() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$SH" \
        SPIRA_RUN="$RUN" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="${TESTDB_BD:-bd}" \
        SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_REPO_MAP="$RMAP" \
        SPIRA_QUEUE_DIR="$QUEUEDIR" \
        SPIRA_FORGE="$SH/forge-fake.sh" \
        FORGE_LOG="$FORGE_LOG" \
        bash "$SH/queue.sh" "$@" 2>&1
}

OPEN_FILE="$QUEUEDIR/$REPONAME/open"
TIP01="aabbcc1100000000000000000000000000000001"
TIP02="ddeeff2200000000000000000000000000000002"
TIP03="eeff003300000000000000000000000000000003"

write_batch() {
    {
        printf 'pr=58\n'
        printf 'head=ffff0000000000000000000000000000000000ff\n'
        printf 'base=0000000000000000000000000000000000000000\n'
        printf 'members=sp-ab01:%s sp-ab02:%s sp-ab03:%s\n' "$TIP01" "$TIP02" "$TIP03"
        printf 'opened=%s\n' "$(date +%s)"
        printf 'branch=spira/queue/20260917T120000Z\n'
    } > "$OPEN_FILE"
}

echo
echo "positive control — no-open-batch is detected (guard is live):"
# The guard must fire when the condition holds; believe it when it is silent.
rm -f "$OPEN_FILE"
out="$(run abandon $REPONAME)"; rc=$?
[ "$rc" -ne 0 ] && ok "exits non-zero when no open batch" || bad "exits non-zero" "rc=$rc"
want "names the repo" "no open batch" "$out"

echo
echo "no open batch: refuses plainly:"
out="$(run abandon $REPONAME)"; rc=$?
[ "$rc" -ne 0 ] && ok "exit non-zero" || bad "exit non-zero" "rc=$rc"
want "message says no open batch" "no open batch for $REPONAME" "$out"

echo
echo "lock held: refuses and changes nothing:"
write_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
printf 'BATCHED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
# Hold the lock externally for the duration of the subshell.
lockfile="$QUEUEDIR/$REPONAME/lock"
exec 8>"$lockfile"
flock 8
out="$(run abandon $REPONAME)"; rc=$?
exec 8>&-
[ "$rc" -ne 0 ] && ok "exit non-zero when lock held" || bad "exit non-zero" "rc=$rc"
want "mentions lock" "holds the lock" "$out"
[ -f "$OPEN_FILE" ] && ok "open file unchanged" || bad "open file unchanged" "file gone"

echo
echo "abandon: PR closed, innocent members CERTIFIED, RED/EJECTED left alone:"
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ab01","title":"ab01","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
{"id":"sp-ab02","title":"ab02","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
{"id":"sp-ab03","title":"ab03","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
JSONL
write_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
# sp-ab03 was deliberately ejected — abandon must not re-certify it.
printf 'EJECTED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
> "$FORGE_LOG"

out="$(run abandon $REPONAME --reason 'guilty branch found')"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0" || bad "exit 0" "rc=$rc out=$out"

# 1. PR closed.
forge_calls="$(cat "$FORGE_LOG" 2>/dev/null || true)"
want "forge pr-comment called"   "pr-comment" "$forge_calls"
want "pr-comment mentions PR 58" "58"          "$forge_calls"
want "forge pr-close called"     "pr-close"    "$forge_calls"

# 2. Innocent members returned to CERTIFIED at their recorded tips.
st01="$(awk '{print $1}' "$LANDSTATE/sp-ab01" 2>/dev/null || true)"
tp01="$(awk '{print $2}' "$LANDSTATE/sp-ab01" 2>/dev/null || true)"
[ "$st01" = "CERTIFIED" ] && ok "sp-ab01 landstate is CERTIFIED" || bad "sp-ab01 CERTIFIED" "got $st01"
[ "$tp01" = "$TIP01"    ] && ok "sp-ab01 tip preserved"          || bad "sp-ab01 tip"       "got $tp01"

st02="$(awk '{print $1}' "$LANDSTATE/sp-ab02" 2>/dev/null || true)"
[ "$st02" = "CERTIFIED" ] && ok "sp-ab02 landstate is CERTIFIED" || bad "sp-ab02 CERTIFIED" "got $st02"

# 3. EJECTED member left untouched.
st03="$(awk '{print $1}' "$LANDSTATE/sp-ab03" 2>/dev/null || true)"
[ "$st03" = "EJECTED"   ] && ok "sp-ab03 left as EJECTED"        || bad "sp-ab03 untouched"  "got $st03"

# 4. Open record gone; archive present with consistent name (trailing Z).
[ ! -f "$OPEN_FILE" ] && ok "open record archived (not open)" || bad "open record gone" "still exists"
archive="$(ls "$QUEUEDIR/$REPONAME/closed-pr58-"* 2>/dev/null | head -1)"
[ -n "$archive" ] && ok "archive exists" || bad "archive exists" "no closed-pr58-* found"
[[ "$archive" == *Z ]] && ok "archive name ends with Z" || bad "archive name Z suffix" "got $archive"

# 5. Reason appears in the pr-comment call.
want "reason in forge call" "guilty branch found" "$forge_calls"

echo
echo "RED member: also left untouched:"
write_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
printf 'RED %s %s\n'     "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
> "$FORGE_LOG"

out="$(run abandon $REPONAME)"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 with RED member" || bad "exit 0 RED" "rc=$rc out=$out"
st03="$(awk '{print $1}' "$LANDSTATE/sp-ab03" 2>/dev/null || true)"
[ "$st03" = "RED" ] && ok "RED member left as RED" || bad "RED untouched" "got $st03"

echo
echo "dry-run: prints plan and changes nothing:"
write_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
printf 'EJECTED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
> "$FORGE_LOG"

out="$(run abandon $REPONAME --dry-run)"; rc=$?
[ "$rc" -eq 0 ] && ok "dry-run exits 0" || bad "dry-run exits 0" "rc=$rc"
want "dry-run mentions PR"          "would close PR 58"                "$out"
want "dry-run innocent member"      "return to CERTIFIED"              "$out"
want "dry-run ejected member"       "leave alone"                      "$out"
want "dry-run shows archive path"   "archive path"                     "$out"

[ -f "$OPEN_FILE" ] && ok "dry-run: open file unchanged" || bad "dry-run no change" "open file gone"
forge_calls="$(cat "$FORGE_LOG" 2>/dev/null || true)"
[ -z "$forge_calls" ] && ok "dry-run: forge not called" || bad "dry-run no forge" "got $forge_calls"
st01="$(awk '{print $1}' "$LANDSTATE/sp-ab01" 2>/dev/null || true)"
[ "$st01" = "BATCHED" ] && ok "dry-run: landstate unchanged" || bad "dry-run landstate" "got $st01"

echo
echo "archive name consistency — always ends with Z:"
write_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ab01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ab02"
printf 'BATCHED %s %s\n' "$TIP03" "$(date +%s)" > "$LANDSTATE/sp-ab03"
run abandon $REPONAME >/dev/null 2>&1 || true
archive="$(ls "$QUEUEDIR/$REPONAME/closed-pr58-"* 2>/dev/null | tail -1)"
[[ "$archive" == *Z ]] && ok "consistent archive name ends with Z" || bad "archive Z suffix" "got $archive"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
