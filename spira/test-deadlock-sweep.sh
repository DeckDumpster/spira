#!/usr/bin/env bash
#
# test-deadlock-sweep.sh — `attempts.sh deadlocked` lists poisoned beads whose work is
# finished and would land cleanly (WOULD), keeps the ones that really failed (KEEP with a
# reason), changes nothing without --apply, and --apply lifts the poison while leaving the
# attempt record standing as history.
#
# Split out of test-requeue.sh (sp-gcx3k, docs/test-plan/aeon-execution.md D15, UC-24): no
# aeon.sh run is needed here — the "deadlock" is a branch that already merges cleanly, built
# with one real commit on a throwaway origin, never a live session.
#
# defect: sp-l7f5
# tier: T2
# covers: spira/attempts.sh spira/lib.sh UC-aeon-execution-24
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-deadlock-sweep
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
# attempts_of/requeues_of read the events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
testdb_up deadlocksweep || {
    printf 'SKIP test-deadlock-sweep: server testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
# attempts.sh's candidates() reads fayth_partitions from the chamber (lib.sh) — the same
# partition builder.fayth declares, so this tool and the summoner cannot disagree about
# which beads are "ours" (attempts.sh's own header comment).
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH

seed() {   # seed <id>
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' "$1" "$_lbl" | testdb_seed
}
cycle() {   # cycle <id> <n> — create n status_changed(in_progress) events via bd update
    local id="$1" n="$2" i=0
    while [ "$i" -lt "$n" ]; do
        bd -C "$SPIRA_DB" update "$id" --status in_progress >/dev/null 2>&1
        bd -C "$SPIRA_DB" update "$id" --status open >/dev/null 2>&1
        i=$((i+1))
    done
}
labels()  { bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | tr '\n' ' '; }
notes()   { bd -C "$SPIRA_DB" show "$1" 2>/dev/null | tr '\n' ' '; }
lib() { bash -c ". \"$SPIRA_HOME/lib.sh\"; $1" 2>/dev/null; }
count_of() { local c; c="$(lib "attempts_of $1")"; printf '%s' "${c:-0}"; }
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SPIRA_HOME/"

echo "test-deadlock-sweep.sh"

echo
echo "the deadlock sweep lifts a poison from finished, landable work:"
testdb_reset; seed sp-rq-s; seed sp-rq-k
# sp-rq-s: poisoned, but its branch names the bead and merges cleanly — the deadlock.
git -C "$REPO" checkout -q -B spira/sp-rq-s origin/main
printf 'finished work\n' > "$REPO/g"; git -C "$REPO" add g
git -C "$REPO" commit -qm "sp-rq-s — the work"
git -C "$REPO" checkout -q main
# sp-rq-k: poisoned with no branch at all, which is a bead that really did fail.
for b in sp-rq-s sp-rq-k; do
    cycle "$b" 3
    bd -C "$SPIRA_DB" label add "$b" spira-poison >/dev/null 2>&1
done
sweep() { SPIRA_HOME="$SPIRA_HOME" SPIRA_RUN="$SPIRA_RUN" SPIRA_DB="$SPIRA_DB" \
          SPIRA_REPO_MAP="$SPIRA_REPO_MAP" SPIRA_REPO="$REPO" SPIRA_FAYTHS=builder \
          bash "$HERE/attempts.sh" deadlocked "$@" 2>&1; }
out="$(sweep)"
want "the deadlocked bead is named"            "WOULD    sp-rq-s" "$out"
want "the genuinely failed one is kept"        "KEEP     sp-rq-k" "$out"
want "with the reason it failed"               "no branch spira/sp-rq-k" "$out"
want "and nothing changed without --apply"     "dry run" "$out"
want "so the poison still stands"              "spira-poison" "$(labels sp-rq-s)"
out="$(sweep --apply)"
want   "the sweep lifts it"                    "RESTORED sp-rq-s" "$out"
nowant "and the label is gone"                 "spira-poison" "$(labels sp-rq-s)"
is     "the rungs are left standing as the record" "3" "$(count_of sp-rq-s)"
want   "the bead records why"                  "Poison lifted by attempts.sh deadlocked" "$(notes sp-rq-s | tr -s ' ')"
want   "the bead that really failed keeps its poison" "spira-poison" "$(labels sp-rq-k)"

tl_summary
