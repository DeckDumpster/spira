#!/usr/bin/env bash
#
# test-batch-express.sh — a certified express branch triggers an immediate batch;
#   a lone non-express branch keeps waiting.
#
# THE PROPERTY UNDER TEST. batch.sh had two triggers (BATCH_MAX and BATCH_WAIT) and
# a third (CI-idle). An express bead is a fourth: any certified branch carrying the
# express label cuts the batch at once, regardless of count or age.
#
# THREE CASES — TWO MUST NOT TRIGGER:
#
#   express      express label present   → batch IS cut before BATCH_MAX or age-out
#   non-express  ordinary label only     → no batch; wait still governs
#   label-off    SPIRA_EXPRESS_LABEL=off → express label does not match; no batch
#
# SEEN RED WITHOUT THE FIX. With the express block removed from batch.sh, the
# "express: batch opened" assertion fails — the express branch sits in CERTIFIED
# without a batch being created. The pair (non-express) passes either way, so both
# must be checked (law-a-regression-test-must-be-seen-to-fail).
#
# covers: spira/batch.sh spira/conf.sh
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
testdb_require test-batch-express
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchexpress || { echo "test-batch-express: could not build fixture database"; exit 1; }

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

# Forge stub: pr-create returns incrementing PR numbers; runs-active returns 1 (CI busy)
# so the CI-idle trigger cannot interfere with these tests. check-status reads
# FORGE_STATUS_FILE (default green, so a scenario that never sets it keeps the
# open-batch guard's default reading as "may still land clean").
FORGE_LOG="$TMP/forge-log"; : > "$FORGE_LOG"
FORGE_STATUS_FILE="$TMP/forge-status"; printf 'green\n' > "$FORGE_STATUS_FILE"
cat > "$SH/forge-fixture.sh" <<FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift; shift  # skip repo arg
case "\$cmd" in
    runs-active) printf '1\n' ;;
    main-gate-status) printf 'green deadbeef\n' ;;
    check-status) cat "$FORGE_STATUS_FILE" 2>/dev/null || printf 'green\n' ;;
    pr-create)
        n=\$(( \$(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "\$n" >> "$FORGE_LOG"
        printf '%s\n' "\$n"
        ;;
    *) exit 0 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

# BATCH_MAX and BATCH_WAIT are huge so the only trigger that can fire is express.
batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=99 \
    SPIRA_QUEUE_BATCH_WAIT=999999 \
    SPIRA_QUEUE_BATCH_IDLE_CUT=0 \
    SPIRA_EXPRESS_LABEL="${EXPRESS_LABEL:-express}" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

# Seed a LANDED anchor so the stuck-queue check has something to read.
NOW="$(date +%s)"
printf 'LANDED fakeshafakeshafakeshafakeshafakeshafake %s\n' "$(( NOW - 60 ))" > "$LANDSTATE/anchor"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-expr1","title":"express bead","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME","express"],"updated_at":"2026-09-23T00:00:00Z"}
{"id":"sp-norm1","title":"normal bead","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-23T00:00:00Z"}
JSONL

# Create both branches; each gets a CERTIFIED landstate entry.
git -C "$REPO" checkout -q -b spira/sp-expr1 main
printf 'e\n' > "$REPO/expr1.txt"
git -C "$REPO" add expr1.txt && git -C "$REPO" commit -q -m "sp-expr1: work"
tip_e="$(git -C "$REPO" rev-parse spira/sp-expr1)"
git -C "$REPO" checkout -q main

git -C "$REPO" checkout -q -b spira/sp-norm1 main
printf 'n\n' > "$REPO/norm1.txt"
git -C "$REPO" add norm1.txt && git -C "$REPO" commit -q -m "sp-norm1: work"
tip_n="$(git -C "$REPO" rev-parse spira/sp-norm1)"
git -C "$REPO" checkout -q main

certify_expr() { printf 'CERTIFIED %s %s' "$tip_e" "$NOW" > "$LANDSTATE/sp-expr1"; }
certify_norm() { printf 'CERTIFIED %s %s' "$tip_n" "$NOW" > "$LANDSTATE/sp-norm1"; }
clear_batch()  {
    rm -f "$QUEUEDIR/$REPONAME/open" "$QUEUEDIR/$REPONAME"/attributing-*
    : > "$FORGE_LOG"
    printf 'green\n' > "$FORGE_STATUS_FILE"
}

# Additional branches for eviction tests.
testdb_seed <<JSONL
{"id":"sp-norm2","title":"normal bead 2","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-23T00:00:00Z"}
{"id":"sp-norm3","title":"normal bead 3","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-23T00:00:00Z"}
JSONL

git -C "$REPO" checkout -q -b spira/sp-norm2 main
printf 'n2\n' > "$REPO/norm2.txt"
git -C "$REPO" add norm2.txt && git -C "$REPO" commit -q -m "sp-norm2: work"
tip_n2="$(git -C "$REPO" rev-parse spira/sp-norm2)"
git -C "$REPO" checkout -q main

git -C "$REPO" checkout -q -b spira/sp-norm3 main
printf 'n3\n' > "$REPO/norm3.txt"
git -C "$REPO" add norm3.txt && git -C "$REPO" commit -q -m "sp-norm3: work"
tip_n3="$(git -C "$REPO" rev-parse spira/sp-norm3)"
git -C "$REPO" checkout -q main

# write_open_batch <pr> <members>: simulate an already-open batch record.
write_open_batch() {
    mkdir -p "$QUEUEDIR/$REPONAME"
    printf 'pr=%s\nmembers=%s\nbranch=spira/queue/fake\nopened=%s\n' \
        "$1" "$2" "$NOW" > "$QUEUEDIR/$REPONAME/open"
}

echo "test-batch-express.sh"

# ── case: express branch — batch IS cut ───────────────────────────────────────
certify_expr; clear_batch; rm -f "$LANDSTATE/sp-norm1"
out="$(batch "$REPONAME")"
want   "express: names the reason"      "express certified branch"   "$out"
is     "express: batch opened"          "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is     "express: landstate BATCHED"     "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-expr1")"

# ── case: non-express branch — wait still governs, no batch ───────────────────
certify_norm; clear_batch; rm -f "$LANDSTATE/sp-expr1"
out="$(batch "$REPONAME")"
nowant "non-express: express reason absent"  "express certified branch" "$out"
is     "non-express: no batch opened"  "0" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is     "non-express: landstate still CERTIFIED" "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-norm1")"

# ── case: express label overridden — branch does not match, no batch ──────────
certify_expr; clear_batch
out="$(EXPRESS_LABEL=other-label batch "$REPONAME")"
nowant "label-off: express reason absent"   "express certified branch" "$out"
is     "label-off: no batch opened"   "0" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"

# ── case: open batch is green-bound — never evicted for express ───────────────
# An express branch certifies while two normal branches are batched and the open
# PR's CI is still green (or pending). The verdict (Ryan, 2026-09-24): a batch
# that might still land clean is not spent to save an express bead a wait.
clear_batch
write_open_batch 99 "sp-norm1:${tip_n} sp-norm2:${tip_n2}"
printf 'BATCHED %s %s\n' "$tip_n"  "$NOW" > "$LANDSTATE/sp-norm1"
printf 'BATCHED %s %s\n' "$tip_n2" "$NOW" > "$LANDSTATE/sp-norm2"
rm -f "$LANDSTATE/sp-expr1" "$LANDSTATE/sp-norm3"
printf 'green\n' > "$FORGE_STATUS_FILE"
certify_expr; : > "$FORGE_LOG"
out="$(batch "$REPONAME")"
nowant "no-evict: no takeover in log"          "takes over"            "$out"
want   "no-evict: skipping message present"    "open batch exists"     "$out"
is     "no-evict: open batch intact"           "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is     "no-evict: no attributing file"         "0" "$(ls "$QUEUEDIR/$REPONAME"/attributing-* 2>/dev/null | wc -l)"
is     "no-evict: norm1 still BATCHED"         "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-norm1")"
is     "no-evict: norm2 still BATCHED"         "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-norm2")"
is     "no-evict: express still CERTIFIED"     "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-expr1")"

# ── pair: once the (green) open batch closes, express takes the next slot ────
# Same setup, but this time the batch has landed (as verdict.sh would leave it):
# no open record, members LANDED. The express branch — still the only thing
# CERTIFIED — gets the very next cut.
printf 'LANDED %s %s\n' "$tip_n"  "$NOW" > "$LANDSTATE/sp-norm1"
printf 'LANDED %s %s\n' "$tip_n2" "$NOW" > "$LANDSTATE/sp-norm2"
rm -f "$QUEUEDIR/$REPONAME/open"
out="$(batch "$REPONAME")"
want "next-slot: names the reason"             "express certified branch" "$out"
is   "next-slot: new batch opened"             "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is   "next-slot: expr1 BATCHED"                "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-expr1")"

# ── case: open batch is red — express takes over without waiting for attribution ─
# The open PR's CI has already resolved red. Waiting for local attribution to
# finish before cutting the express batch would defeat the fast lane, so the
# express branch takes the freed slot immediately; the old record is stashed
# for verdict.sh to attribute on its own (test-verdict.sh case 33).
clear_batch
write_open_batch 77 "sp-norm1:${tip_n} sp-norm2:${tip_n2}"
printf 'BATCHED %s %s\n' "$tip_n"  "$NOW" > "$LANDSTATE/sp-norm1"
printf 'BATCHED %s %s\n' "$tip_n2" "$NOW" > "$LANDSTATE/sp-norm2"
rm -f "$LANDSTATE/sp-expr1" "$LANDSTATE/sp-norm3"
printf 'red\n' > "$FORGE_STATUS_FILE"
certify_expr; : > "$FORGE_LOG"
out="$(batch "$REPONAME")"
want "takeover: names the reason"              "express branch takes over" "$out"
nowant "takeover: no eviction wording"         "evicting"                  "$out"
is   "takeover: stashed for attribution"       "1" "$(ls "$QUEUEDIR/$REPONAME/attributing-77" 2>/dev/null | wc -l)"
is   "takeover: stashed record keeps its members" "sp-norm1:${tip_n} sp-norm2:${tip_n2}" \
     "$(grep '^members=' "$QUEUEDIR/$REPONAME/attributing-77" | cut -d= -f2-)"
is   "takeover: old members untouched (still BATCHED)" "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-norm1")"
is   "takeover: new batch opened"              "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is   "takeover: expr1 BATCHED in new batch"    "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-expr1")"
is   "takeover: new batch has only the express member" "sp-expr1:${tip_e}" \
     "$(grep '^members=' "$QUEUEDIR/$REPONAME/open" | cut -d= -f2-)"

# ── pair: non-express certifies while the open batch is red — no takeover ────
# Only express bypasses the open-batch wait; an ordinary certification must not.
clear_batch
write_open_batch 77 "sp-norm1:${tip_n} sp-norm2:${tip_n2}"
printf 'BATCHED %s %s\n' "$tip_n"  "$NOW" > "$LANDSTATE/sp-norm1"
printf 'BATCHED %s %s\n' "$tip_n2" "$NOW" > "$LANDSTATE/sp-norm2"
rm -f "$LANDSTATE/sp-expr1"
printf 'CERTIFIED %s %s' "$tip_n3" "$NOW" > "$LANDSTATE/sp-norm3"
printf 'red\n' > "$FORGE_STATUS_FILE"
: > "$FORGE_LOG"
out="$(batch "$REPONAME")"
nowant "nontakeover: no takeover in log"       "takes over"            "$out"
want   "nontakeover: skipping message present" "open batch exists"     "$out"
is     "nontakeover: open batch intact"        "1" "$(ls "$QUEUEDIR/$REPONAME/open" 2>/dev/null | wc -l)"
is     "nontakeover: no attributing file"      "0" "$(ls "$QUEUEDIR/$REPONAME"/attributing-* 2>/dev/null | wc -l)"
is     "nontakeover: norm1 still BATCHED"      "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-norm1")"
is     "nontakeover: norm2 still BATCHED"      "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-norm2")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
