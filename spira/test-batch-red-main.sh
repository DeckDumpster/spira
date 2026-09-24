#!/usr/bin/env bash
#
# test-batch-red-main.sh — the queue holds a batch while main's most recent push
#   gate is red, except when a certified member carries SPIRA_RED_MAIN_LABEL.
#
# THE PROPERTY UNDER TEST (sp-221n8). The batch PR and the push to main ran
# different suite sets, so a batch could go green and land while the push gate
# that actually tests main afterward failed — and nothing then stopped the next
# batch landing on top of that red main. This suite covers the second half of
# the fix: even a batch that would otherwise cut (express, BATCH_MAX, ...) must
# not cut while main's last push gate is red, unless it is carrying the fix.
#
# FOUR CASES:
#
#   green    main-gate-status reports green   → batch cuts normally
#   red      main-gate-status reports red,
#            no member carries the label      → HELD; landing.log says why
#   red-fix  main-gate-status reports red,
#            a member carries the label       → batch cuts anyway
#   unknown  main-gate-status cannot tell      → HELD (fail-closed, same as red)
#
# SEEN RED WITHOUT THE FIX. With the red-main guard removed from batch.sh, the
# "red" case cuts a batch exactly like "green" — the HOLD assertions fail
# (law-a-regression-test-must-be-seen-to-fail).
#
# tier: T2
# covers: spira/batch.sh spira/forge.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batch-red-main
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchredmain || { echo "test-batch-red-main: could not build fixture database"; exit 1; }

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
cp "$HERE"/*.sh "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
exit 0
MAIL
chmod +x "$SH/mail.sh"

# Forge stub: main-gate-status reports whatever FIXTURE_GATE_STATUS says (default
# green); runs-active returns 1 (CI busy) so the CI-idle trigger never fires here.
FORGE_LOG="$TMP/forge-log"; : > "$FORGE_LOG"
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; shift  # skip repo arg
case "$cmd" in
    runs-active) printf '1\n' ;;
    main-gate-status) printf '%s\n' "${FIXTURE_GATE_STATUS:-green deadbeef}" ;;
    pr-create)
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n" >> "$FORGE_LOG"
        printf '%s\n' "$n"
        ;;
    *) exit 0 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

# BATCH_MAX and BATCH_WAIT are huge; express is the only ordinary trigger allowed
# to fire, so every case here is testing what the red-main guard does to a batch
# that has already been triggered.
batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=99 \
    SPIRA_QUEUE_BATCH_WAIT=999999 \
    SPIRA_QUEUE_BATCH_IDLE_CUT=0 \
    SPIRA_EXPRESS_LABEL=express \
    SPIRA_RED_MAIN_LABEL="${RED_MAIN_LABEL:-fixes-red-main}" \
    FIXTURE_GATE_STATUS="${GATE_STATUS:-green deadbeef}" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

# Seed a LANDED anchor so the stuck-queue check has something to read.
NOW="$(date +%s)"
printf 'LANDED fakeshafakeshafakeshafakeshafakeshafake %s\n' "$(( NOW - 60 ))" > "$LANDSTATE/anchor"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-plain1","title":"plain express bead","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME","express"],"updated_at":"2026-09-23T00:00:00Z"}
{"id":"sp-fixmain","title":"fixes red main","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME","express","fixes-red-main"],"updated_at":"2026-09-23T00:00:00Z"}
JSONL

git -C "$REPO" checkout -q -b spira/sp-plain1 main
printf 'p\n' > "$REPO/plain1.txt"
git -C "$REPO" add plain1.txt && git -C "$REPO" commit -q -m "sp-plain1: work"
tip_p="$(git -C "$REPO" rev-parse spira/sp-plain1)"
git -C "$REPO" checkout -q main

git -C "$REPO" checkout -q -b spira/sp-fixmain main
printf 'f\n' > "$REPO/fixmain.txt"
git -C "$REPO" add fixmain.txt && git -C "$REPO" commit -q -m "sp-fixmain: work"
tip_f="$(git -C "$REPO" rev-parse spira/sp-fixmain)"
git -C "$REPO" checkout -q main

certify_plain() { printf 'CERTIFIED %s %s' "$tip_p" "$NOW" > "$LANDSTATE/sp-plain1"; }
certify_fix()   { printf 'CERTIFIED %s %s' "$tip_f" "$NOW" > "$LANDSTATE/sp-fixmain"; }
clear_batch()   { rm -f "$QUEUEDIR/$REPONAME/open"; : > "$FORGE_LOG"; }
landing_log()   { cat "$RUN/landing.log" 2>/dev/null; }
clear_log()     { : > "$RUN/landing.log"; }

echo "test-batch-red-main.sh"

# ── case: green — batch cuts normally, no HOLD ────────────────────────────────
certify_plain; clear_batch; clear_log; rm -f "$LANDSTATE/sp-fixmain"
out="$(GATE_STATUS='green deadbeef' batch "$REPONAME")"
is     "green: batch opened"            "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is     "green: landstate BATCHED"       "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-plain1")"
nowant "green: no HOLD in landing.log"  "QUEUE HOLD"  "$(landing_log)"

# ── case: red, no fixing member — batch is HELD ───────────────────────────────
certify_plain; clear_batch; clear_log; rm -f "$LANDSTATE/sp-fixmain"
out="$(GATE_STATUS='red badc0de' batch "$REPONAME")"
want "red: names the hold"              "HOLD"                      "$out"
is   "red: no batch opened"             "0" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is   "red: landstate still CERTIFIED"   "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-plain1")"
want "red: landing.log records the hold" "QUEUE HOLD"               "$(landing_log)"
want "red: landing.log names the reason" "reason=red-main"          "$(landing_log)"

# ── case: red, a certified member carries the fix label — batch cuts anyway ───
certify_fix; clear_batch; clear_log; rm -f "$LANDSTATE/sp-plain1"
out="$(GATE_STATUS='red badc0de' batch "$REPONAME")"
want "red-fix: names landing anyway"    "landing anyway"            "$out"
is   "red-fix: batch opened"            "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is   "red-fix: landstate BATCHED"       "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-fixmain")"
nowant "red-fix: no HOLD in landing.log" "QUEUE HOLD"                "$(landing_log)"

# ── case: unknown gate status — HELD, same as red (fail-closed) ───────────────
certify_plain; clear_batch; clear_log; rm -f "$LANDSTATE/sp-fixmain"
out="$(GATE_STATUS='unknown' batch "$REPONAME")"
want "unknown: names the hold"          "HOLD"                      "$out"
is   "unknown: no batch opened"         "0" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
want "unknown: landing.log records the hold" "QUEUE HOLD"           "$(landing_log)"

# ── pair: red-main label overridden — plain express bead no longer matches
#          even carrying the default label name is not enough once the
#          configured label changes; the guard reads SPIRA_RED_MAIN_LABEL, not
#          a hardcoded string.
certify_fix; clear_batch; clear_log; rm -f "$LANDSTATE/sp-plain1"
out="$(GATE_STATUS='red badc0de' RED_MAIN_LABEL='other-label' batch "$REPONAME")"
want "label-off: names the hold"        "HOLD"                      "$out"
is   "label-off: no batch opened"       "0" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"

tl_summary
