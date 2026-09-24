#!/usr/bin/env bash
#
# test-batch-landed-false.sh — batch.sh sweeps LANDED landstate records whose tip never
#   reached base, reports every one, and recovers what it can.
#
# THE DEFECT (sp-dgaig). Two writers used to put a bead's landstate at LANDED without the
# tip's content ever having reached the base branch: a REMOVED reap-log line read alone
# (fixed in batch.sh's orphan-reconciliation block; see test-batch-certified-orphan.sh) and
# landed()'s old any-mention match (fixed in lib.sh; see test-check5-body-search.sh). Five
# certified branches were reaped and marked LANDED this way, and being closed beads, nothing
# else noticed. This suite covers the third leg of the fix: a sweep that runs every batch
# pass and finds any LANDED record left in that state, however it got there.
#
# THREE CASES, THREE OUTCOMES:
#
#   sp-landed-ok:  LANDED, tip IS an ancestor of base — legitimate. No WARN, no mail, the
#                  record is untouched (positive control: the sweep does not misfire).
#
#   sp-recover:    LANDED, tip is NOT on base, but a branch for it still exists on the
#                  repository's own remote (the local ref is gone — the exact shape the
#                  concierge recovered sp-yivi7 from by hand). The sweep restores the local
#                  ref and re-certifies so the queue can retry the real work.
#
#   sp-lost:       LANDED, tip is NOT on base, and no branch exists anywhere. The sweep can
#                  only report it — recovery needs a human to say where the work went.
#
# SEEN RED WITHOUT THE FIX. Without _landed_false in batch.sh, none of sp-recover's or
# sp-lost's WARN lines are produced, no mail is sent, and sp-recover's landstate stays
# LANDED instead of being restored to CERTIFIED; the "want"/"is" assertions below then fail.
#
# defect: sp-dgaig
# covers: spira/batch.sh spira/conf.sh spira/landing.sh
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
testdb_require test-batch-landed-false
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchlandedfalse || { echo "test-batch-landed-false: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"
MAIL_LOG="$TMP/mail-log"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

cat > "$SH/mail.sh" <<MAIL
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$MAIL_LOG"
MAIL
chmod +x "$SH/mail.sh"
: > "$MAIL_LOG"

FORGE_LOG="$TMP/forge-log"
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    main-gate-status) printf 'green deadbeef\n' ;;
    pr-create)
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n" >> "$FORGE_LOG"
        printf '%s\n' "$n"
        ;;
    *) printf 'forge-fixture: unknown: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
: > "$FORGE_LOG"

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=8 \
    SPIRA_QUEUE_BATCH_WAIT=0 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

testdb_reset
testdb_seed <<JSONL
{"id":"sp-good","title":"certified branch with ref","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-17T00:00:00Z"}
JSONL

# ─── Fixture: sp-good — a normal CERTIFIED branch, so batch has real work to do ───────
git -C "$REPO" checkout -q -b spira/sp-good main
printf 'good\n' > "$REPO/sp-good.txt"
git -C "$REPO" add sp-good.txt && git -C "$REPO" commit -q -m "sp-good: work"
_good_tip="$(git -C "$REPO" rev-parse spira/sp-good)"
git -C "$REPO" checkout -q main
printf 'CERTIFIED %s %s' "$_good_tip" "$(date +%s)" > "$LANDSTATE/sp-good"

# ─── Fixture: sp-landed-ok — LANDED, tip IS on base (legitimate; must not be touched) ──
git -C "$REPO" checkout -q -b spira/sp-landed-ok main
printf 'ok\n' > "$REPO/sp-landed-ok.txt"
git -C "$REPO" add sp-landed-ok.txt && git -C "$REPO" commit -q -m "spira: land sp-landed-ok"
git -C "$REPO" checkout -q main
git -C "$REPO" merge -q --ff-only spira/sp-landed-ok
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
_ok_tip="$(git -C "$REPO" rev-parse spira/sp-landed-ok)"
git -C "$REPO" branch -D spira/sp-landed-ok >/dev/null
printf 'LANDED %s %s' "$_ok_tip" "$(date +%s)" > "$LANDSTATE/sp-landed-ok"

# ─── Fixture: sp-recover — LANDED, tip NOT on base, branch survives on the remote only ─
git -C "$REPO" checkout -q -b spira/sp-recover main
printf 'unlanded-recover\n' > "$REPO/sp-recover.txt"
git -C "$REPO" add sp-recover.txt && git -C "$REPO" commit -q -m "sp-recover: real unlanded work"
git -C "$REPO" push -q origin spira/sp-recover
_recover_tip="$(git -C "$REPO" rev-parse spira/sp-recover)"
git -C "$REPO" checkout -q main
git -C "$REPO" branch -D spira/sp-recover >/dev/null
git -C "$REPO" fetch -q origin
printf 'LANDED %s %s' "$_recover_tip" "$(date +%s)" > "$LANDSTATE/sp-recover"

# ─── Fixture: sp-lost — LANDED, tip NOT on base, no branch anywhere ────────────────────
printf 'LANDED fakeshafakeshabrakeshabrakebrakefakeshabrakebra %s' "$(date +%s)" \
    > "$LANDSTATE/sp-lost"

echo "test-batch-landed-false.sh"

# =============================================================================
echo
echo "batch runs normally with sp-good as the only real member:"
# =============================================================================
out="$(batch "$REPONAME")"
rc=$?
is  "batch exits 0"                       0 "$rc"
want "batch opens a PR"                   "PR" "$out"
want "sp-good is a batch member"          "sp-good" "$(cat "$QUEUEDIR/$REPONAME/open" 2>/dev/null)"

# =============================================================================
echo
echo "positive control — sp-landed-ok (tip on base) is not reported or touched:"
# =============================================================================
nowant "no false-landed WARN for sp-landed-ok" "false-landed sp-landed-ok" "$out"
is "sp-landed-ok landstate is unchanged" \
    "LANDED $_ok_tip" "$(cat "$LANDSTATE/sp-landed-ok" 2>/dev/null | cut -d' ' -f1-2)"

# =============================================================================
echo
echo "sp-recover — LANDED but unlanded, branch found on the remote → RE-CERTIFIED:"
# =============================================================================
want "batch logs WARN for false-landed sp-recover" \
    "false-landed sp-recover" "$out"
want "batch reports sp-recover re-certified" \
    "RE-CERTIFIED" "$(printf '%s\n' "$out" | grep sp-recover)"
case "$(cat "$LANDSTATE/sp-recover" 2>/dev/null)" in CERTIFIED*)
    ok "sp-recover landstate restored to CERTIFIED" ;;
    *) bad "sp-recover landstate restored to CERTIFIED" \
           "got: $(cat "$LANDSTATE/sp-recover" 2>/dev/null)" ;; esac
git -C "$REPO" show-ref --verify -q "refs/heads/spira/sp-recover" \
    && ok "sp-recover branch restored locally" \
    || bad "sp-recover branch restored locally" "refs/heads/spira/sp-recover is still missing"

# =============================================================================
echo
echo "sp-lost — LANDED but unlanded, no branch anywhere → reported, left alone:"
# =============================================================================
want "batch logs WARN for false-landed sp-lost" \
    "false-landed sp-lost" "$out"
want "batch reports sp-lost needs manual recovery" \
    "needs manual recovery" "$(printf '%s\n' "$out" | grep sp-lost)"
case "$(cat "$LANDSTATE/sp-lost" 2>/dev/null)" in LANDED*)
    ok "sp-lost landstate left alone (still LANDED, flagged not silently changed)" ;;
    *) bad "sp-lost landstate left alone" \
           "got: $(cat "$LANDSTATE/sp-lost" 2>/dev/null)" ;; esac

want "mail.sh was called with the false-landed subject" \
    "LANDED record" "$(cat "$MAIL_LOG" 2>/dev/null)"

printf '\n%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
