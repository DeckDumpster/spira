#!/usr/bin/env bash
#
# test-batch-idle-cut.sh — batch.sh cuts a batch without waiting when CI is idle, and
#   refuses to when CI is busy or when it cannot tell.
#
# THE PROPERTY UNDER TEST. batch.sh had two triggers: SPIRA_QUEUE_BATCH_MAX certified
# branches, or the oldest having waited SPIRA_QUEUE_BATCH_WAIT seconds. The wait buys one
# thing — several branches sharing a CI run instead of spending one each — and that trade
# only pays while a run is in flight. With nothing in CI it is pure delay, which is how a
# certified P0 sat idle while the machine that would test it sat idle too. The operator,
# verbatim (2026-09-23): "if there's NOTHING in CI, then we may as well just send an
# immediate batch."
#
# FOUR CASES, AND THREE OF THEM MUST NOT CUT:
#
#   idle       runs-active prints 0   → batch IS cut although the wait is nowhere near up
#   busy       runs-active prints 3   → no batch; the wait still governs
#   unknown    runs-active prints ?   → NO BATCH. This is the one that matters. A forge that
#              cannot be reached must not read as an empty queue, or the trigger fires on
#              every pass exactly when the forge is down (law-absence-needs-a-positive-control)
#   disabled   SPIRA_QUEUE_BATCH_IDLE_CUT=0, CI idle → no batch; the switch turns it off
#
# SEEN RED WITHOUT THE FIX. With the idle block removed from batch.sh, case "idle" opens no
# PR and its assertion fails. With the `case` arm written as a bare numeric test rather than
# one that rejects non-digits, case "unknown" opens a PR and ITS assertion fails. Both were
# run red before this landed.
#
# covers: spira/batch.sh spira/conf.sh spira/forge.sh
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
testdb_require test-batch-idle-cut
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchidlecut || { echo "test-batch-idle-cut: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"; QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
exit 0
MAIL
chmod +x "$SH/mail.sh"

# Forge stub. RUNS_ACTIVE is what `runs-active` prints; the test sets it per case.
FORGE_LOG="$TMP/forge-log"
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    runs-active) printf '%s\n' "${RUNS_ACTIVE:-?}" ;;
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

# WAIT IS HUGE IN EVERY CASE. If any batch is cut here it is because of the idle trigger and
# nothing else — the age trigger cannot fire inside a test that runs in seconds.
batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=99 \
    SPIRA_QUEUE_BATCH_WAIT=999999 \
    SPIRA_QUEUE_BATCH_IDLE_CUT="${IDLE_CUT:-1}" \
    RUNS_ACTIVE="${RUNS_ACTIVE:-?}" \
    FORGE_LOG="$FORGE_LOG" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

testdb_reset
testdb_seed <<JSONL
{"id":"sp-idle1","title":"certified and waiting","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-23T00:00:00Z"}
JSONL

git -C "$REPO" checkout -q -b spira/sp-idle1 main
printf 'work\n' > "$REPO/sp-idle1.txt"
git -C "$REPO" add sp-idle1.txt && git -C "$REPO" commit -q -m "sp-idle1: work"
_tip="$(git -C "$REPO" rev-parse spira/sp-idle1)"
git -C "$REPO" checkout -q main

recertify() { printf 'CERTIFIED %s %s' "$_tip" "$(date +%s)" > "$LANDSTATE/sp-idle1"; }
clear_batch() { rm -f "$QUEUEDIR/$REPONAME/open"; : > "$FORGE_LOG"; }

echo "test-batch-idle-cut.sh"

# ── case: CI busy — the wait still governs ───────────────────────────────────
recertify; clear_batch
out="$(RUNS_ACTIVE=3 batch "$REPONAME")"
nowant "busy: no PR opened"            "PR "                  "$out"
nowant "busy: idle line absent"        "CI idle"              "$out"
is     "busy: no open batch recorded"  "0" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is     "busy: landstate still CERTIFIED" "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-idle1")"

# ── case: cannot tell — MUST behave as busy, never as idle ───────────────────
recertify; clear_batch
out="$(RUNS_ACTIVE='?' batch "$REPONAME")"
nowant "unknown: no PR opened"         "PR "                  "$out"
nowant "unknown: idle line absent"     "CI idle"              "$out"
is     "unknown: landstate still CERTIFIED" "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-idle1")"

# ── case: garbage — also must not cut ────────────────────────────────────────
recertify; clear_batch
out="$(RUNS_ACTIVE='not-a-number' batch "$REPONAME")"
nowant "garbage: no PR opened"         "PR "                  "$out"

# ── case: switch off — idle but disabled ─────────────────────────────────────
recertify; clear_batch
out="$(RUNS_ACTIVE=0 IDLE_CUT=0 batch "$REPONAME")"
nowant "disabled: no PR opened"        "PR "                  "$out"
nowant "disabled: idle line absent"    "CI idle"              "$out"

# ── case: CI idle — THE NEW BEHAVIOUR ────────────────────────────────────────
recertify; clear_batch
out="$(RUNS_ACTIVE=0 batch "$REPONAME")"
want   "idle: batch opened"            "PR 1 opened"          "$out"
want   "idle: names the reason"        "CI idle"              "$out"
want   "idle: names the count"         "0 runs queued or in progress" "$out"
is     "idle: landstate now BATCHED"   "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-idle1")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
