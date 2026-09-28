#!/usr/bin/env bash
#
# test-deadlock-sweep.sh — `attempts.sh deadlocked` lists poisoned beads whose work is
# finished and would land cleanly (WOULD), keeps the ones that really failed (KEEP with a
# reason), changes nothing without --apply, and --apply lifts the poison while leaving the
# attempt record standing as history.
#
# Split out of test-requeue.sh (sp-g44ke, docs/test-plan/aeon-execution.md D15, UC-24): no
# aeon.sh run is needed here — the "deadlock" is a branch that already merges cleanly, built
# with one real commit on a throwaway origin, never a live session.
#
# THE HOLD, NOT THE LABEL (sp-i2m7y): poisoned() (attempts.sh) reads a real spira-lc hold,
# so this suite starts its own throwaway `dolt sql-server` for spira_lifecycle and builds a
# real spira-lc binary, the same shape test-lc-hold.sh and test-check2-reaper.sh use — a
# stub lc_hold/lc_held would only prove this suite's own model of spira-lc agrees with
# itself.
#
# tier: T2
# covers: spira/attempts.sh spira/lib.sh spira/lc.sh UC-aeon-execution-24
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# Resolved BEFORE conf.sh (sourced transitively by testdb.sh below), which can overwrite
# PATH with the harness's own tool directories first — see test-poison.sh's own note.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin"
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-deadlock-sweep
TMP="$(mktemp -d)"
LC_SERVER_PID=""
trap 'testdb_drop; [ -n "$LC_SERVER_PID" ] && kill "$LC_SERVER_PID" >/dev/null 2>&1; rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
export PATH="$(dirname "$CARGO_BIN"):$(dirname "$DOLT_BIN"):$PATH"
unset SPIRA_LC_SOCKET
# attempts_of/requeues_of read the events table via bd sql, which embedded mode refuses.
# testdb-mode: server — attempts_of/requeues_of read the events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
testdb_up deadlocksweep || {
    printf 'SKIP test-deadlock-sweep: server testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# ── a throwaway spira-lc/Dolt server, the same shape test-lc-hold.sh uses ─────────────
SRC_ROOT="$(cd "$HERE/.." && pwd)"
LC_PORT=$((SPIRA_LC_TESTDB_PORT + 1600 + (RANDOM % 300)))
LC_TMP="$TMP/lc"; mkdir -p "$LC_TMP/data"
cat > "$LC_TMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $LC_PORT
  max_connections: 50
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$LC_TMP/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
"$DOLT_BIN" sql-server --config "$LC_TMP/server.yaml" > "$LC_TMP/server.log" 2>&1 &
LC_SERVER_PID=$!
lc_up=0
for _ in $(seq 1 50); do
    if "$DOLT_BIN" --data-dir "$LC_TMP" --host 127.0.0.1 --port "$LC_PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        lc_up=1; break
    fi
    sleep 0.2
done
[ "$lc_up" = 1 ] || bail "dolt sql-server for spira_lifecycle never came up: $(cat "$LC_TMP/server.log")"

LC_CARGO_TARGET="$LC_TMP/cargo-target"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$LC_CARGO_TARGET" \
    "$CARGO_BIN" build --manifest-path "$SRC_ROOT/spira-lc/Cargo.toml" --quiet 2>"$LC_TMP/build.log" \
    || bail "spira-lc failed to build: $(cat "$LC_TMP/build.log")"
export SPIRA_LC_BIN="$LC_CARGO_TARGET/debug/spira-lc"
export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$LC_PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$LC_TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
"$SPIRA_LC_BIN" admin-apply-ddl "$SRC_ROOT/lifecycle/schema.sql" >"$LC_TMP/schema.log" 2>&1
wantrc "spira_lifecycle schema applies cleanly" 0 $?
# shellcheck disable=SC1090
. "$HERE/lc.sh"
# mkpoison <id> — a fresh READY row, then a real Hold{Poison} event through lc_hold, the
# same shape test-poison.sh's own mkpoison uses.
mkpoison() {
    "$SPIRA_LC_BIN" create-bead "$1" >/dev/null 2>&1
    lc_hold "$1" poison "seed" test >/dev/null 2>&1
}
lcheld() { lc_held "$1" poison; }

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
    mkpoison "$b"
done
sweep() { SPIRA_HOME="$SPIRA_HOME" SPIRA_RUN="$SPIRA_RUN" SPIRA_DB="$SPIRA_DB" \
          SPIRA_REPO_MAP="$SPIRA_REPO_MAP" SPIRA_REPO="$REPO" SPIRA_FAYTHS=builder \
          bash "$HERE/attempts.sh" deadlocked "$@" 2>&1; }
out="$(sweep)"
want "the deadlocked bead is named"            "WOULD    sp-rq-s" "$out"
want "the genuinely failed one is kept"        "KEEP     sp-rq-k" "$out"
want "with the reason it failed"               "no branch spira/sp-rq-k" "$out"
want "and nothing changed without --apply"     "dry run" "$out"
is   "so the poison still stands"              0 "$(lcheld sp-rq-s; echo $?)"
out="$(sweep --apply)"
want "the sweep lifts it"                      "RESTORED sp-rq-s" "$out"
is   "and the hold is gone"                    1 "$(lcheld sp-rq-s; echo $?)"
is     "the rungs are left standing as the record" "3" "$(count_of sp-rq-s)"
want   "the bead records why"                  "Poison lifted by attempts.sh deadlocked" "$(notes sp-rq-s | tr -s ' ')"
is   "the bead that really failed keeps its poison" 0 "$(lcheld sp-rq-k; echo $?)"

tl_summary
