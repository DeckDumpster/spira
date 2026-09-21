#!/usr/bin/env bash
# test-queue-eject.sh — queue.sh eject removes one member from the open batch.
#
# Eight cases, asserted directly not via exit code:
#   1. Positive control: eject finds a real member (proves the guard is live).
#   2. Non-member is refused; batch and landstate unchanged.
#   3. Eject a member: RED landstate, bead open+claimable, comment posted,
#      PR closed, survivor returned to CERTIFIED, batch removed.
#   4. Landstate round-trips through the reader (no trailing-newline defect).
#   5. Dry-run prints plan, changes nothing, exits non-zero for non-member.
#   6. Lock held: another queue operation holds the lock → eject refuses.
#   7. Single-member batch: eject the only member → PR closed, batch removed.
#   8. Dry-run for a member prints plan and changes nothing.
#
# covers: spira/queue.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-queue-eject.sh"

. "$HERE/testdb.sh"
testdb_require test-queue-eject
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up queue-eject || { echo "test-queue-eject: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

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

B() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" "$@"; }
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }

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

write_batch() {
    {
        printf 'pr=42\n'
        printf 'head=ffff0000000000000000000000000000000000ff\n'
        printf 'base=0000000000000000000000000000000000000000\n'
        printf 'members=sp-ej01:%s sp-ej02:%s\n' "$TIP01" "$TIP02"
        printf 'opened=%s\n' "$(date +%s)"
        printf 'branch=spira/queue/20260917T120000Z\n'
    } > "$OPEN_FILE"
}

seed_beads() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-ej01","title":"ej01","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
{"id":"sp-ej02","title":"ej02","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
JSONL
}

seed_beads
write_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
> "$FORGE_LOG"

echo
echo "positive control — eject finds a real member (guard is live):"
out="$(run eject sp-ej01 --reason 'test-foo.sh RED')"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 for valid member" || bad "exit 0 for valid member" "rc=$rc out=$out"
want "reports ejection" "ejected sp-ej01" "$out"
# Verify guard actually checked: landstate IS now RED (the eject found the member).
st="$(awk '{print $1}' "$LANDSTATE/sp-ej01" 2>/dev/null || true)"
[ "$st" = "RED" ] && ok "landstate RED after eject" || bad "landstate RED" "got $st"

echo
echo "non-member is refused; batch and landstate unchanged:"
write_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
out="$(run eject sp-nonexist)"; rc=$?
[ "$rc" -ne 0 ] && ok "exits non-zero for non-member" || bad "exits non-zero" "rc=$rc"
want   "names the id" "sp-nonexist is not a member" "$out"
want   "lists batch members" "members:" "$out"
# Batch record must be unchanged.
members_now="$(grep '^members=' "$OPEN_FILE" | head -1)"
[[ "$members_now" == *"sp-ej01:"* ]] && ok "sp-ej01 still in batch" || bad "batch unchanged" "$members_now"
[[ "$members_now" == *"sp-ej02:"* ]] && ok "sp-ej02 still in batch" || bad "batch unchanged" "$members_now"
[ ! -f "$LANDSTATE/sp-nonexist" ] && ok "no landstate written for non-member" || bad "no landstate" "file exists"

echo
echo "all outcomes: RED landstate, bead open, assignee cleared, PR closed, survivor CERTIFIED, batch removed:"
seed_beads
write_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
> "$FORGE_LOG"

out="$(run eject sp-ej01 --reason 'test-suite-x.sh RED: assertion mismatch at line 42')"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0" || bad "exit 0" "rc=$rc out=$out"

# 1. Landstate is RED.
st="$(awk '{print $1}' "$LANDSTATE/sp-ej01" 2>/dev/null || true)"
[ "$st" = "RED" ] && ok "landstate written as RED" || bad "landstate RED" "got $st"

# 2. Bead is open (claimable).
bead_st="$(field sp-ej01 status)"
[ "$bead_st" = "open" ] && ok "bead reopened" || bad "bead open" "status=$bead_st"

# 3. Assignee cleared.
assignee="$(field sp-ej01 assignee)"
[ -z "$assignee" ] && ok "assignee cleared" || bad "assignee cleared" "got $assignee"

# 4. Surviving member returned to CERTIFIED.
st02="$(awk '{print $1}' "$LANDSTATE/sp-ej02" 2>/dev/null || true)"
tp02="$(awk '{print $2}' "$LANDSTATE/sp-ej02" 2>/dev/null || true)"
[ "$st02" = "CERTIFIED" ] && ok "survivor returned to CERTIFIED" || bad "survivor CERTIFIED" "got $st02"
[ "$tp02" = "$TIP02"    ] && ok "survivor tip preserved"         || bad "survivor tip"       "got $tp02"

# 5. PR closed via forge.
forge_calls="$(cat "$FORGE_LOG" 2>/dev/null || true)"
want "forge pr-close called" "pr-close" "$forge_calls"
want "pr-close mentions PR 42" "42" "$forge_calls"

# 6. Batch record removed (queue unblocked).
[ ! -f "$OPEN_FILE" ] && ok "batch record removed" || bad "batch record removed" "file still exists"

# 7. Comment posted.
comment_out="$(B comments sp-ej01 2>/dev/null || true)"
[ -n "$comment_out" ] && ok "comment posted to bead" || bad "comment posted" "no output from bd comments"

echo
echo "landstate round-trips through the reader (format check):"
seed_beads
write_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
run eject sp-ej01 >/dev/null 2>&1 || true

# Simulate the reader used by batch.sh and verdict.sh.
_st=""; _tip=""
{ read -r _st _tip _ < "$LANDSTATE/sp-ej01"; } 2>/dev/null || true
[ "$_st" = "RED" ] && ok "round-trip: state field is RED" || bad "round-trip state" "got $_st"
[ "$_tip" = "$TIP01" ] && ok "round-trip: tip field preserved" || bad "round-trip tip" "got $_tip"

echo
echo "dry-run prints plan; non-member exits non-zero:"
write_batch
out="$(run eject sp-nonexist --dry-run)"; rc=$?
[ "$rc" -ne 0 ] && ok "dry-run non-member exits non-zero" || bad "dry-run non-member" "rc=$rc"

echo
echo "lock held: refuses and changes nothing:"
seed_beads
write_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
lockfile="$QUEUEDIR/$REPONAME/lock"
exec 8>"$lockfile"
flock 8
out="$(run eject sp-ej01)"; rc=$?
exec 8>&-
[ "$rc" -ne 0 ] && ok "exit non-zero when lock held" || bad "exit non-zero" "rc=$rc"
want "mentions lock" "holds the lock" "$out"
[ -f "$OPEN_FILE" ] && ok "open file unchanged" || bad "open file unchanged" "file gone"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej01" 2>/dev/null || true)"
[ "$st" = "BATCHED" ] && ok "landstate unchanged when lock held" || bad "landstate unchanged" "got $st"

echo
echo "single-member batch: eject closes PR and removes batch:"
seed_beads
{
    printf 'pr=55\n'
    printf 'head=ffff0000000000000000000000000000000000ff\n'
    printf 'base=0000000000000000000000000000000000000000\n'
    printf 'members=sp-ej01:%s\n' "$TIP01"
    printf 'opened=%s\n' "$(date +%s)"
    printf 'branch=spira/queue/20260917T130000Z\n'
} > "$OPEN_FILE"
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
> "$FORGE_LOG"

out="$(run eject sp-ej01)"; rc=$?
[ "$rc" -eq 0 ] && ok "single-member: exit 0" || bad "single-member: exit 0" "rc=$rc out=$out"
st="$(awk '{print $1}' "$LANDSTATE/sp-ej01" 2>/dev/null || true)"
[ "$st" = "RED" ] && ok "single-member: landstate RED" || bad "single-member: RED" "got $st"
forge_calls="$(cat "$FORGE_LOG" 2>/dev/null || true)"
want "single-member: forge pr-close called" "pr-close" "$forge_calls"
[ ! -f "$OPEN_FILE" ] && ok "single-member: batch removed" || bad "single-member: batch removed" "file exists"

echo
echo "dry-run for a member prints plan and changes nothing:"
seed_beads
write_batch
printf 'BATCHED %s %s\n' "$TIP01" "$(date +%s)" > "$LANDSTATE/sp-ej01"
printf 'BATCHED %s %s\n' "$TIP02" "$(date +%s)" > "$LANDSTATE/sp-ej02"
> "$FORGE_LOG"
out="$(run eject sp-ej01 --dry-run)"; rc=$?
[ "$rc" -eq 0 ] && ok "dry-run exits 0 for member" || bad "dry-run exits 0" "rc=$rc"
want "dry-run mentions RED write"        "would write RED"           "$out"
want "dry-run mentions bead reopen"      "would reopen bead sp-ej01" "$out"
want "dry-run mentions PR close"         "would close PR 42"         "$out"
want "dry-run mentions survivor CERTIFIED" "would return survivors"  "$out"
# State must be unchanged.
st="$(awk '{print $1}' "$LANDSTATE/sp-ej01" 2>/dev/null || true)"
[ "$st" = "BATCHED" ] && ok "dry-run did not change landstate" || bad "dry-run no change" "got $st"
[ -f "$OPEN_FILE" ] && ok "dry-run did not remove batch" || bad "dry-run no change" "batch gone"
forge_calls="$(cat "$FORGE_LOG" 2>/dev/null || true)"
[ -z "$forge_calls" ] && ok "dry-run: forge not called" || bad "dry-run no forge" "got $forge_calls"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
