#!/usr/bin/env bash
#
# test-batch-red-main.sh — the queue holds the LANDING while main's push gate is
#   red or unknown, but never holds the CUT for a gate that is merely still
#   running; either hold lifts for a batch carrying SPIRA_RED_MAIN_LABEL.
#
# THE PROPERTY UNDER TEST (sp-221n8, then sp-wmn0w). The batch PR and the push
# to main ran different suite sets, so a batch could go green and land while
# the push gate that actually tests main afterward failed — and nothing then
# stopped the next batch landing on top of that red main. sp-221n8 fixed that
# by holding the queue on anything but green, but "unknown" and "still running"
# were the same case: every batch CUT then waited out main's ~20-30 minute
# full-corpus gate before its own CI could even start, serialising the two.
#
# THREE STATES AT CUT TIME (batch.sh):
#
#   green    main-gate-status reports green    → batch cuts normally
#   pending  a gate run for main is queued or
#            in progress                       → batch cuts anyway; landing.log
#                                                 says the landing will wait
#   red      main-gate-status reports red,
#            no member carries the label       → HELD; landing.log says why
#   red-fix  main-gate-status reports red,
#            a member carries the label        → batch cuts anyway
#   unknown  main-gate-status cannot tell       → HELD (fail-closed, same as red)
#
# AND AGAIN AT LANDING TIME (verdict.sh), against a batch that already cut
# green: a base gate that was pending at cut time may have resolved by the time
# this batch's own CI finishes, so verdict.sh reads it fresh right before the
# fast-forward push — pending waits, red holds (unless a member fixes it),
# unknown holds, green lands.
#
# SEEN RED WITHOUT THE FIX. With the red-main guard removed, the "red" cases
# cut/land exactly like "green" — the HOLD assertions fail
# (law-a-regression-test-must-be-seen-to-fail).
#
# covers: spira/batch.sh spira/verdict.sh spira/forge.sh spira/conf.sh
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
# check-status reports whatever FIXTURE_CHECK_STATUS says (default green) — the
# batch's OWN CI result, which the landing-time cases hold constant at green so
# they exercise only the base-gate check, never the batch's own pending/red path.
FORGE_LOG="$TMP/forge-log"; : > "$FORGE_LOG"
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; shift  # skip repo arg
case "$cmd" in
    runs-active) printf '1\n' ;;
    main-gate-status) printf '%s\n' "${FIXTURE_GATE_STATUS:-green deadbeef}" ;;
    check-status) printf '%s\n' "${FIXTURE_CHECK_STATUS:-green}" ;;
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

# verdict() drives the LANDING half (verdict.sh) against whatever batch is
# currently open — built by a real batch() cut above. FIXTURE_CHECK_STATUS is
# pinned to green: these cases test only what verdict does with the BASE gate
# (FIXTURE_GATE_STATUS), never the batch's own CI result.
verdict() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_CI_MAXSEC=3600 \
    SPIRA_QUEUE_CI_IDLE_SEC=600 \
    SPIRA_QUEUE_INFRA_RETRIES=2 \
    SPIRA_RED_MAIN_LABEL="${RED_MAIN_LABEL:-fixes-red-main}" \
    FIXTURE_GATE_STATUS="${GATE_STATUS:-green deadbeef}" \
    FIXTURE_CHECK_STATUS="${CHECK_STATUS:-green}" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/verdict.sh" "$@" 2>&1
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
remote_main()   { git -C "$REMOTE" rev-parse main 2>/dev/null; }

# build_batch <id:tip> [<id:tip> ...] — write an open batch record directly
# (bypassing batch.sh's own gate/cut) so the LANDING cases below can drive
# verdict.sh against a known batch shape without paying for a real gate run
# or risking a same-second branch-name collision from repeated real cuts.
# Writes BATCHED landstate for each member and prints the batch head sha.
build_batch() {
    local base_sha; base_sha="$(git -C "$REPO" rev-parse origin/main)"
    local wt="$RUN/worktree/.batch-build"
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    git -C "$REPO" worktree add -q --detach "$wt" "$base_sha"
    local members=() spec id tip now; now="$(date +%s)"
    for spec in "$@"; do
        id="${spec%%:*}"; tip="${spec##*:}"
        git -C "$wt" merge -q --no-edit --no-ff -m "spira: land $id" "$tip" >/dev/null 2>&1
        members+=("$id:$tip")
        printf 'BATCHED %s %s\n' "$tip" "$now" > "$LANDSTATE/$id"
    done
    local batch_head; batch_head="$(git -C "$wt" rev-parse HEAD)"
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    local branch="spira/queue/build-$$-${RANDOM}"
    git -C "$REPO" branch -f "$branch" "$batch_head" 2>/dev/null
    git -C "$REPO" push -q origin "$branch" 2>/dev/null
    {
        printf 'pr=99\n'
        printf 'head=%s\n'    "$batch_head"
        printf 'base=%s\n'    "$base_sha"
        printf 'members=%s\n' "${members[*]}"
        printf 'opened=%s\n'  "$now"
        printf 'branch=%s\n'  "$branch"
    } > "$QUEUEDIR/$REPONAME/open"
    printf '%s\n' "$batch_head"
}

echo "test-batch-red-main.sh"

# ── case: green — batch cuts normally, no HOLD ────────────────────────────────
certify_plain; clear_batch; clear_log; rm -f "$LANDSTATE/sp-fixmain"
out="$(GATE_STATUS='green deadbeef' batch "$REPONAME")"
is     "green: batch opened"            "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is     "green: landstate BATCHED"       "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-plain1")"
nowant "green: no HOLD in landing.log"  "QUEUE HOLD"  "$(landing_log)"

# ── case: pending — base gate still running — batch cuts, no HOLD ─────────────
# sp-wmn0w: PENDING must never hold the cut. Otherwise every batch's own CI is
# serialised behind main's, for a state that is not red.
certify_plain; clear_batch; clear_log; rm -f "$LANDSTATE/sp-fixmain"
out="$(GATE_STATUS='pending abc123' batch "$REPONAME")"
is     "pending: batch opened"           "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is     "pending: landstate BATCHED"      "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-plain1")"
nowant "pending: no HOLD in landing.log" "QUEUE HOLD"           "$(landing_log)"
want   "pending: names cut-and-wait"     "cut, landing waits"  "$out"
want   "pending: landing.log records it" "reason=base-gate-pending" "$(landing_log)"

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

# =============================================================================
# LANDING (verdict.sh): hold the LANDING, not the cut. These cases drive a
# batch that already cut clean (build_batch, not batch.sh — see its comment)
# through verdict.sh under each base-gate state.
# =============================================================================

# ── case: base gate pending at landing time — waits, then lands once green ────
clear_batch; clear_log
before_main="$(remote_main)"
batch_head_p="$(build_batch "sp-plain1:$tip_p")"
out="$(GATE_STATUS='pending abc123' CHECK_STATUS='green' verdict "$REPONAME")"
is   "landing-pending: no push while pending" "$before_main" "$(remote_main)"
is   "landing-pending: batch stays open"      "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
want "landing-pending: names the wait"        "landing waits" "$out"
nowant "landing-pending: no HOLD logged"      "QUEUE HOLD" "$(landing_log)"

out="$(GATE_STATUS='green cafefe0' CHECK_STATUS='green' verdict "$REPONAME")"
is   "landing-pending: lands once gate turns green" "$batch_head_p" "$(remote_main)"
is   "landing-pending: batch closed"                "0" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is   "landing-pending: sp-plain1 LANDED" "LANDED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-plain1")"
want "landing-pending: landed reported"  "landed by fast-forward" "$out"
git -C "$REPO" fetch -q origin

# ── case: base gate red at landing time, no fixing member — HELD ──────────────
clear_batch; clear_log
before_main="$(remote_main)"
build_batch "sp-plain1:$tip_p" > /dev/null
out="$(GATE_STATUS='red badc0de' CHECK_STATUS='green' verdict "$REPONAME")"
is   "landing-red: no push"           "$before_main" "$(remote_main)"
is   "landing-red: batch stays open"  "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
want "landing-red: names the hold"    "base gate red"  "$out"
want "landing-red: landing.log HOLD"  "QUEUE HOLD"     "$(landing_log)"
want "landing-red: landing.log names the reason" "reason=red-main" "$(landing_log)"

# ── case: base gate red at landing time, a member carries the fix label ───────
clear_batch; clear_log
batch_head_rf="$(build_batch "sp-fixmain:$tip_f")"
out="$(GATE_STATUS='red badc0de' CHECK_STATUS='green' verdict "$REPONAME")"
is   "landing-red-fix: lands anyway"       "$batch_head_rf" "$(remote_main)"
want "landing-red-fix: names landing anyway" "landing anyway" "$out"
nowant "landing-red-fix: no HOLD logged"     "QUEUE HOLD"     "$(landing_log)"
git -C "$REPO" fetch -q origin

# ── case: base gate unknown at landing time — HELD (fail-closed) ──────────────
clear_batch; clear_log
before_main="$(remote_main)"
build_batch "sp-plain1:$tip_p" > /dev/null
out="$(GATE_STATUS='unknown (forge unreachable)' CHECK_STATUS='green' verdict "$REPONAME")"
is   "landing-unknown: no push"          "$before_main" "$(remote_main)"
is   "landing-unknown: batch stays open" "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
want "landing-unknown: names the hold"   "base gate unknown" "$out"
want "landing-unknown: landing.log HOLD" "QUEUE HOLD"        "$(landing_log)"
clear_batch

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
