#!/usr/bin/env bash
# test-queue-ops.sh — queue.sh eject and abandon against a real bd, which the queue crate's
# fakes cannot be: that an eject reopens the bead, clears the assignee and lands a comment,
# and that abandon's return-to-CERTIFIED path makes no bd call at all. The decision rows
# (membership, locks, dry-run, unreachable store, run-cancel failure) are queue tests.
#
# tier: T2
# covers: queue/src/* forge/src/* spira/conf.sh UC-landing-merge-queue-29
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testlib/lc-fixture.sh"

echo "test-queue-ops.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-queue-ops

TMP="$(mktemp -d)"; trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
lcfix_up || { echo "test-queue-ops: could not build a lifecycle fixture"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixq
QUEUEDIR="$RUN/queue"
FORGE_LOG="$TMP/forge.log"
RUNS_FILE="$TMP/runs-for-branch"
CANCEL_FAIL="$TMP/cancel-fail"
BD_LOG="$TMP/bd-calls.log"

git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m init
mkdir -p "$RUN/worktree" "$SH" "$QUEUEDIR/$REPONAME"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SH/"

# Fake forge: records arguments, succeeds silently. runs-for-branch answers from
# RUNS_FILE (empty/absent = no in-flight runs); run-cancel fails when CANCEL_FAIL
# exists, so the retry/loud-failure path can be exercised.
cat > "$SH/forge-fake.sh" <<ENDFAKE
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "\$FORGE_LOG"
case "\${1:-}" in
    runs-for-branch) [ -f "\$RUNS_FILE" ] && cat "\$RUNS_FILE"; exit 0 ;;
    run-cancel)      [ -f "\$CANCEL_FAIL" ] && exit 1; exit 0 ;;
esac
exit 0
ENDFAKE
chmod +x "$SH/forge-fake.sh"

# Recording bd stub: logs every call, answers `show` positively for any id so the
# eject dry-run's existence guard passes, and no-ops everything else. Good enough for
# every case that never reads bd state back — which is every case except the real-bd
# section below.
cat > "$SH/bd-stub.sh" <<'BDSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${BD_LOG:?}"
# A comment body arrives on stdin (`comment <id> --stdin`): record it too, so a case can
# assert what the comment says, not only that one was posted.
case " $* " in *" --stdin "*) cat >> "$BD_LOG"; printf '\n' >> "$BD_LOG" ;; esac
# The queue binary reads `bd -C <db> show <id> --json` rows (an exit 0 with no row is
# "cannot resolve"), so show answers one JSON row per id asked for.
[ "${1:-}" = -C ] && shift 2
case "${1:-}" in
    show) shift; sep='['; for a in "$@"; do case "$a" in -*) ;; *) printf '%s{"id":"%s"}' "$sep" "$a"; sep=',' ;; esac; done; printf ']\n'; exit 0 ;;
    *)    exit 0 ;;
esac
BDSTUB
chmod +x "$SH/bd-stub.sh"

LC_DOWN="SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT=1 SPIRA_LC_DB=spira_lifecycle SPIRA_LC_USER=root"
RMAP="$TMP/repo-map"
printf '%s | %s | queue | main | | |\n' "$REPONAME" "$REPO" > "$RMAP"

run() {
    # SPIRA_RUN/SPIRA_DB/SPIRA_BD/SPIRA_HOME_REPO/SPIRA_REPO_MAP/SPIRA_QUEUE_DIR/
    # SPIRA_FORGE are registered keys (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare
    # via tl_config and thread SPIRA_TOML through env -i, which clears it.
    tl_config SPIRA_RUN="$RUN" SPIRA_DB="${SPIRA_DB:-/nonexistent}" SPIRA_BD="$SH/bd-stub.sh" \
        SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$RMAP" SPIRA_QUEUE_DIR="$QUEUEDIR" \
        SPIRA_FORGE="$SH/forge-fake.sh"
    env -i ${LCENV:-$(lcfix_env)} PATH="$SH:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$SH" \
        BD_LOG="$BD_LOG" \
        FORGE_LOG="$FORGE_LOG" \
        RUNS_FILE="$RUNS_FILE" \
        CANCEL_FAIL="$CANCEL_FAIL" \
        BEADS_ACTOR="dwarf-abandontest" \
        SPIRA_EVENT_COOLDOWN=0 \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$SH" queue "$@" 2>&1
}

TIP01="aabbcc1100000000000000000000000000000001"
TIP02="ddeeff2200000000000000000000000000000002"
TIP03="eeff003300000000000000000000000000000003"

write_eject_batch() {
    {
        printf 'pr=42\n'
        printf 'head=ffff0000000000000000000000000000000000ff\n'
        printf 'base=0000000000000000000000000000000000000000\n'
        printf 'members=sp-ej01:%s sp-ej02:%s\n' "$TIP01" "$TIP02"
        printf 'opened=%s\n' "$(date +%s)"
        printf 'branch=spira/queue/20260917T120000Z\n'
    } > "$OPEN_FILE"
}

BATCH_SEQ=0
BATCH_ID=""
lc_reset() { local t; for t in event delivery batch_member batch bead; do lcfix_sql -q "DELETE FROM $t" >/dev/null 2>&1; done; }
batch_state() { lcfix_sql -q "SELECT state FROM batch WHERE batch_id='$1'" -r csv 2>/dev/null | sed -n 2p; }
seed_eject() {
    lcfix_seed sp-ej01 IN_DELIVERY "$TIP01"
    lcfix_seed sp-ej02 IN_DELIVERY "$TIP02"
}
seed_abandon() {
    lcfix_seed sp-ab01 IN_DELIVERY "$TIP01"
    lcfix_seed sp-ab02 IN_DELIVERY "$TIP02"
    lcfix_seed sp-ab03 IN_DELIVERY "$TIP03"
}

# write_abandon_batch [cut] — the open record; with "cut" the three members are CERTIFIED
# rows cut into a real batch on spira-lc and the record names it.
write_abandon_batch() {
    local extra=""
    if [ "${1:-}" = cut ]; then
        BATCH_SEQ=$((BATCH_SEQ + 1)); BATCH_ID="$REPONAME-20260917T13000${BATCH_SEQ}Z"
        lc_reset
        lcfix_seed sp-ab01 CERTIFIED "$TIP01"
        lcfix_seed sp-ab02 CERTIFIED "$TIP02"
        lcfix_seed sp-ab03 CERTIFIED "$TIP03"
        spira-lc cut "$BATCH_ID" --repo "$REPONAME" --head ffff0000000000000000000000000000000000ff \
            --base 0000000000000000000000000000000000000000 \
            --members "sp-ab01:$TIP01,sp-ab02:$TIP02,sp-ab03:$TIP03" --actor fixture >/dev/null 2>&1
        extra="batch_id=$BATCH_ID"
    fi
    {
        printf 'pr=58\n'
        printf 'head=ffff0000000000000000000000000000000000ff\n'
        printf 'base=0000000000000000000000000000000000000000\n'
        printf 'members=sp-ab01:%s sp-ab02:%s sp-ab03:%s\n' "$TIP01" "$TIP02" "$TIP03"
        printf 'opened=%s\n' "$(date +%s)"
        printf 'branch=spira/queue/20260917T130000Z\n'
        [ -n "$extra" ] && printf '%s\n' "$extra"
    } > "$OPEN_FILE"
}

OPEN_FILE="$QUEUEDIR/$REPONAME/open"

# =============================================================================
# EJECT — recording-bd stub, real spira-lc (UC-29).
# =============================================================================

# =============================================================================
# REAL BD — one fixture database, two things a stub cannot verify:
#  * eject reopens the bead, clears the assignee, and posts a comment (UC-29/30).
#  * G7 — abandon's return-to-CERTIFIED path makes NO bd call at all: status and
#    assignee for the surviving members are untouched. A stub always answers "ok",
#    so only a real bd, read back afterwards, can catch an accidental bdq call here.
# =============================================================================

echo
echo "real bd: eject clears assignee, posts a comment, leaves bd status unmoved:"
testdb_up queueops || { echo "test-queue-ops: could not build fixture database"; exit 1; }
B() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" "$@"; }
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }

real_run() {
    # SPIRA_RUN/SPIRA_DB/SPIRA_BD/SPIRA_HOME_REPO/SPIRA_REPO_MAP/SPIRA_QUEUE_DIR/
    # SPIRA_FORGE are registered keys (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare
    # via tl_config and thread SPIRA_TOML through env -i, which clears it.
    tl_config SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD:-bd}" \
        SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO_MAP="$RMAP" SPIRA_QUEUE_DIR="$QUEUEDIR" \
        SPIRA_FORGE="$SH/forge-fake.sh"
    env -i $(lcfix_env) PATH="$SH:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" \
        FORGE_LOG="$FORGE_LOG" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$SH" queue "$@" 2>&1
}

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ej01","title":"ej01","status":"closed","issue_type":"task","labels":[],"assignee":"dwarf-someone","updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
{"id":"sp-ej02","title":"ej02","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
{"id":"sp-ab01","title":"ab01","status":"closed","issue_type":"task","labels":[],"assignee":"dwarf-holder","updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
{"id":"sp-ab02","title":"ab02","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
JSONL

write_eject_batch
seed_eject
> "$FORGE_LOG"

out="$(real_run eject sp-ej01 --reason 'test-suite-x.sh RED: assertion mismatch at line 42')"; rc=$?
[ "$rc" -eq 0 ] && ok "real bd: eject exit 0" || bad "real bd: eject exit 0" "rc=$rc out=$out"
is "real bd: the ejected bead is REWORK on spira-lc" "REWORK" "$(lcfix_state sp-ej01)"

bead_st="$(field sp-ej01 status)"
[ "$bead_st" = "closed" ] && ok "real bd: bd status is left unmoved by eject" || bad "real bd: bd status unmoved" "status=$bead_st"
# (No assignee check: a batch eject hands the bead back on its lifecycle row; bd's assignee is
# content no claim reads, and no reopen path writes it any more — sp-swh8b8.)
comment_out="$(B comments sp-ej01 2>/dev/null || true)"
[ -n "$comment_out" ] && ok "real bd: comment posted to bead" || bad "real bd: comment posted" "no output from bd comments"

echo
echo "real bd: eject on a CERTIFIED, unbatched bead withdraws it (no open batch at all):"
testdb_seed <<'JSONL'
{"id":"sp-ej-cert","title":"ej-cert","status":"closed","issue_type":"task","labels":[],"assignee":"dwarf-someone-else","updated_at":"2026-09-17T00:00:00Z","closed_at":"2026-09-17T00:00:00Z"}
JSONL
rm -f "$OPEN_FILE"
lcfix_seed sp-ej-cert CERTIFIED "$TIP03"

out="$(real_run eject sp-ej-cert --reason 'holding for a fix')"; rc=$?
[ "$rc" -eq 0 ] && ok "real bd: eject exit 0 for certified, unbatched bead" || bad "real bd: eject exit 0" "rc=$rc out=$out"
bead_st="$(field sp-ej-cert status)"
[ "$bead_st" = "open" ] && ok "real bd: certified-unbatched bead reopened" || bad "real bd: bead open" "status=$bead_st"
assignee="$(field sp-ej-cert assignee)"
[ -z "$assignee" ] && ok "real bd: certified-unbatched assignee cleared" || bad "real bd: assignee cleared" "got $assignee"
comment_out2="$(B comments sp-ej-cert 2>/dev/null || true)"
[ -n "$comment_out2" ] && ok "real bd: comment posted to certified-unbatched bead" || bad "real bd: comment posted" "no output"
is "real bd: the withdrawn bead is REWORK on spira-lc" "REWORK" "$(lcfix_state sp-ej-cert)"

echo
echo "real bd — gap G7: abandon's return-to-CERTIFIED path makes no bd call at all:"
write_abandon_batch cut
{
    printf 'pr=91\n'
    printf 'head=ffff0000000000000000000000000000000000ff\n'
    printf 'base=0000000000000000000000000000000000000000\n'
    printf 'members=sp-ab01:%s sp-ab02:%s\n' "$TIP01" "$TIP02"
    printf 'opened=%s\n' "$(date +%s)"
    printf 'branch=spira/queue/20260917T140000Z\n'
    printf 'batch_id=%s\n' "$BATCH_ID"
} > "$OPEN_FILE"
> "$FORGE_LOG"

out="$(real_run abandon $REPONAME --reason 'gap G7 probe')"; rc=$?
[ "$rc" -eq 0 ] && ok "real bd: abandon exit 0" || bad "real bd: abandon exit 0" "rc=$rc out=$out"

is "real bd: sp-ab01 returned to CERTIFIED" "CERTIFIED" "$(lcfix_state sp-ab01)"
is "real bd: the batch is ABANDONED" "ABANDONED" "$(batch_state "$BATCH_ID")"

ab01_st="$(field sp-ab01 status)"
[ "$ab01_st" = "closed" ] && ok "G7: sp-ab01 bead status untouched by abandon (still closed)" \
    || bad "G7: sp-ab01 status untouched" "got $ab01_st"
ab01_assignee="$(field sp-ab01 assignee)"
[ "$ab01_assignee" = "dwarf-holder" ] && ok "G7: sp-ab01 assignee untouched by abandon" \
    || bad "G7: sp-ab01 assignee untouched" "got $ab01_assignee"
ab01_comments="$(B comments sp-ab01 2>/dev/null || true)"
want "G7: abandon posted no comment to sp-ab01" "No comments" "$ab01_comments"

echo
tl_summary
