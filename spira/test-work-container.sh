#!/usr/bin/env bash
#
# test-work-container.sh — the aeon semantic layer end to end: a real spira_lifecycle
# (its own throwaway dolt sql-server), a real bd (testdb.sh), and real mail.sh, driven
# through the built `work` client over `spira-lc serve`'s socket exactly as an aeon's
# restricted environment would reach it.
#
# WHAT THIS PROVES (acceptance criteria): each verb emits exactly the documented event or
# filing, and a verb naming any bead other than the bound one is refused even when a real
# socket answers (test-work-crate.sh already proves the refusal never needs one).
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as
# test-lifecycle-container.sh; testenv-batch.sh already provides the container.
#
# tier: T2
# covers: work/* spira-lc/src/work.rs spira-lc/src/bd.rs spira/bead.sh spira/mail.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin"
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

. "$HERE/conf.sh"
export PATH="$(dirname "$CARGO_BIN"):$(dirname "$DOLT_BIN"):$PATH"

. "$HERE/testdb.sh"
testdb_require "test-work-container.sh"

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
PORT=$((SPIRA_LC_TESTDB_PORT + 700 + (RANDOM % 500)))
SERVER_PID=""
SERVE_PID=""

cleanup() {
    [ -n "$SERVE_PID" ] && kill "$SERVE_PID" >/dev/null 2>&1
    [ -n "$SERVER_PID" ] && kill "$SERVER_PID" >/dev/null 2>&1
    testdb_drop >/dev/null 2>&1
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

testdb_up "test-work-container"

# A real mailbox root and kind set, never the operator's real maildir.
export SPIRA_MAIL="$TMP/mail"
export SPIRA_MAIL_KINDS="$TMP/kinds"
mkdir -p "$SPIRA_MAIL_KINDS"
cp -r "$HERE/mail/kinds/." "$SPIRA_MAIL_KINDS/"

# A repo-map fixture, never the host's real one: repo-map.example (the shipped fallback)
# names no repo this suite could use, and the real map is host-specific inventory.
# Empty lanes column admits every lane, so builder's plan partition is always granted.
cat > "$TMP/repo-map" <<MAP
testrepo | $TMP | push | origin/main | | |
MAP
export SPIRA_REPO_MAP="$TMP/repo-map"

mkdir -p "$TMP/data"
cat > "$TMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $PORT
  max_connections: 100
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$TMP/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML

"$DOLT_BIN" sql-server --config "$TMP/server.yaml" > "$TMP/server.log" 2>&1 &
SERVER_PID=$!

up=0
for _ in $(seq 1 50); do
    if "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        up=1; break
    fi
    sleep 0.2
done
[ "$up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/server.log")"

root_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls "$@"; }

# Same pin as test-lifecycle-container.sh: this suite runs inside testenv-batch.sh's own
# podman exec, which sets its own CARGO_TARGET_DIR.
CARGO_TARGET_DIR_FOR_BUILD="$TMP/cargo-target"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
    "$CARGO_BIN" build --manifest-path "$REPO/spira-lc/Cargo.toml" --quiet 2>"$TMP/build-lc.log" \
    || bail "spira-lc failed to build: $(cat "$TMP/build-lc.log")"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
    "$CARGO_BIN" build --manifest-path "$REPO/work/Cargo.toml" --quiet 2>"$TMP/build-work.log" \
    || bail "work failed to build: $(cat "$TMP/build-work.log")"
LC_BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/spira-lc"
WORK_BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/work"

# `work` is the lifecycle machine's door: its subject is lifecycle-ON behaviour. OFF (the
# default) refuses every verb before the socket — covered by work/src/lib.rs unit tests and
# test-work-crate.sh.
export SPIRA_LIFECYCLE_ENFORCE=1
export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""

"$LC_BIN" admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?
# Grants restrict the spira_lc user specifically; this suite connects as root throughout
# (SPIRA_LC_USER=root above) to exercise the verbs themselves, not the grant boundary —
# that is test-lifecycle-container.sh's job — so grants.sql (which needs its
# @SPIRA_LC_PASSWORD@ placeholder filled first) is not applied here.

seed_bead() {   # seed_bead <bead-id>
    root_sql --use-db spira_lifecycle sql -q \
        "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES ('$1','READY','[]',0,0)" >/dev/null 2>&1
}

SOCK="$TMP/spira-lc.sock"
SPIRA_LC_SOCKET="$SOCK" "$LC_BIN" serve "$SOCK" >"$TMP/serve.log" 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 50); do
    [ -S "$SOCK" ] && break
    sleep 0.1
done
[ -S "$SOCK" ] || bail "spira-lc serve never created its socket: $(cat "$TMP/serve.log")"
export SPIRA_LC_SOCKET="$SOCK"

# ── one real bd bead, filed through bead.sh's own contract, and its lifecycle twin ───
BID="$(bash "$HERE/bead.sh" file "aeon semantic layer container-tier fixture" --for builder --repo testrepo)"
[ -n "$BID" ] || bail "bead.sh file did not return an id"
seed_bead "$BID"

work_as() {   # work_as <bead-id> <verb> [args...]
    local bid="$1"; shift
    SPIRA_WORK_BEAD_ID="$bid" SPIRA_FAYTH="builder" SPIRA_LC_SOCKET="$SOCK" "$WORK_BIN" "$@"
}

# ── show: reads bd content ────────────────────────────────────────────────────────────
out="$(work_as "$BID" show 2>&1)"; rc=$?
is   "show: exits 0"                 "0"                                              "$rc"
want "show: bd's own title is in it" "aeon semantic layer container-tier fixture"     "$out"

# ── note: a plain bd note, no lifecycle event ────────────────────────────────────────
before_events="$(root_sql --use-db spira_lifecycle sql -q "SELECT COUNT(*) AS n FROM event WHERE lc_key='$BID'" -r json 2>&1)"
out="$(work_as "$BID" note "left by the container-tier suite" 2>&1)"; rc=$?
is "note: exits 0" "0" "$rc"
note_text="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$BID" 2>&1)"
want "note: text landed on the bd bead" "left by the container-tier suite" "$note_text"
after_events="$(root_sql --use-db spira_lifecycle sql -q "SELECT COUNT(*) AS n FROM event WHERE lc_key='$BID'" -r json 2>&1)"
is "note: emits no lifecycle event (bd, non-lifecycle, per design §3.5)" "$before_events" "$after_events"

# submit is legal only from WORKING (lifecycle/src/bead.rs) — seed_bead leaves BID at
# READY, same as every other fixture bead here; move it to WORKING first.
root_sql --use-db spira_lifecycle sql -q "UPDATE bead SET state='WORKING', holder='aeon-test', version=1 WHERE bead_id='$BID'" >/dev/null 2>&1

# ── submit: bead Submit(tip), tip read from the worktree, never taken as an argument ──
# A throwaway repo, never $REPO: $REPO here is testenv-batch.sh's own bind-mounted worktree,
# whose .git gitlink points at a commondir that exists on the host, not inside this
# container, so `git rev-parse HEAD` against it fails closed with no tip at all.
AEON_WT="$TMP/aeon-worktree"
mkdir -p "$AEON_WT"
git -C "$AEON_WT" init -q
git -C "$AEON_WT" -c user.email=aeon@spira.local -c user.name=aeon commit -q --allow-empty -m "fixture commit"
real_tip="$(git -C "$AEON_WT" rev-parse HEAD)"
out="$(cd "$AEON_WT" && work_as "$BID" submit 2>&1)"; rc=$?
is "submit: exits 0 (applied)" "0" "$rc"
row="$(root_sql --use-db spira_lifecycle sql -q "SELECT state, tip FROM bead WHERE bead_id='$BID'" -r json 2>&1)"
want "submit: state is SUBMITTED" "SUBMITTED" "$row"
want "submit: tip is the worktree's real HEAD, not something the aeon could type" "$real_tip" "$row"

# ── done needs WORKING, not SUBMITTED — reset via a fresh bead for the rest ──────────
DID="$(bash "$HERE/bead.sh" file "aeon semantic layer: done/blocked/split fixture" --for builder --repo testrepo)"
seed_bead "$DID"
root_sql --use-db spira_lifecycle sql -q "UPDATE bead SET state='WORKING', holder='aeon-test', version=1 WHERE bead_id='$DID'" >/dev/null 2>&1

out="$(work_as "$DID" done --delivers "a document at wiki/x" 2>&1)"; rc=$?
is "done: exits 0 (applied)" "0" "$rc"
row="$(root_sql --use-db spira_lifecycle sql -q "SELECT state, reason FROM bead WHERE bead_id='$DID'" -r json 2>&1)"
want "done: state is DONE" "\"state\":\"DONE\"" "$row"
want "done: reason carries the delivers evidence" "a document at wiki/x" "$row"

# ── blocked: a hold, plus an ask filed for the operator ──────────────────────────────
BLID="$(bash "$HERE/bead.sh" file "aeon semantic layer: blocked fixture" --for builder --repo testrepo)"
seed_bead "$BLID"
before_unread="$(ls "$SPIRA_MAIL/operator/new" 2>/dev/null | wc -l)"
out="$(work_as "$BLID" blocked "which persona owns this?" --default "builder" 2>&1)"; rc=$?
is "blocked: exits 0" "0" "$rc"
row="$(root_sql --use-db spira_lifecycle sql -q "SELECT holds FROM bead WHERE bead_id='$BLID'" -r json 2>&1)"
want "blocked: an ask hold is recorded" "ask" "$row"
after_unread="$(ls "$SPIRA_MAIL/operator/new" 2>/dev/null | wc -l)"
[ "$after_unread" -gt "$before_unread" ] && ok "blocked: an ask landed in the operator's mailbox" \
    || bad "blocked: an ask landed in the operator's mailbox" "count did not increase ($before_unread -> $after_unread)"

# ── file-followup: filed through bead.sh's contract, parent set by the layer ─────────
out="$(work_as "$BID" file-followup "a followup filed by the container-tier suite" 2>&1)"; rc=$?
is "file-followup: exits 0" "0" "$rc"
new_id="$(printf '%s' "$out" | tail -n1 | tr -d '[:space:]')"
[ -n "$new_id" ] || bail "file-followup produced no new bead id"
child_json="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$new_id" --json 2>&1)"
want "file-followup: the new bead's parent is the bound bead, not asked for" "\"parent\": \"$BID\"" "$child_json"
nowant "file-followup: did not inherit the bound bead's branch label" "branch:" "$child_json"

# ── split: same mechanism, its own bead ───────────────────────────────────────────────
out="$(work_as "$BID" split "a split piece filed by the container-tier suite" 2>&1)"; rc=$?
is "split: exits 0" "0" "$rc"

# ── superseded-by: a hold plus a confirmation ask, never bd supersede directly ───────
SBID="$(bash "$HERE/bead.sh" file "aeon semantic layer: superseded-by fixture" --for builder --repo testrepo)"
seed_bead "$SBID"
out="$(work_as "$SBID" superseded-by "$BID" 2>&1)"; rc=$?
is "superseded-by: exits 0" "0" "$rc"
row="$(root_sql --use-db spira_lifecycle sql -q "SELECT state, holds, reason FROM bead WHERE bead_id='$SBID'" -r json 2>&1)"
want   "superseded-by: an operator hold is recorded, not SUPERSEDED" "operator" "$row"
nowant "superseded-by: the bead itself is not moved to SUPERSEDED"   "\"state\":\"SUPERSEDED\"" "$row"
want   "superseded-by: the request names the proposed successor"    "$BID" "$row"

# ── acceptance criterion, at full stack: a verb naming a foreign bead is refused ─────
out="$(work_as "$BID" note "$SBID" 2>&1)"; rc=$?
is "foreign bead reference: refused (exit 3) even with a live socket and a live server" "3" "$rc"

tl_summary
