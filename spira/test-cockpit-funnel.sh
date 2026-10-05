#!/usr/bin/env bash
#
# test-cockpit-funnel.sh — DONE→LANDED pipeline funnel in the QUEUE section.
#
# Three scenarios:
#   (a) fixture with the exact shapes from the bead description, now spira-lc rows instead
#       of landstate files: done=3 (no row), certify=2 (SUBMITTED, legacy GATED), red=5
#       (timeout/rebase/gate/conflict — REWORK with a GateRedReason breakdown), certified=1
#       (CERTIFIED). Verifies counts, breakdown, oldest ages, pane rendering.
#   (b) spira-lc unreachable — all funnel keys must be ?, pane shows ?.
#   (c) positive control — funnel keys absent from env renders ? in the pane (not 0).
#
# tier: T2
# covers: cockpit-collect/src/* cockpit/ops/src/health.rs spira-lc/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"
testdb_require cockpit-funnel
testdb_up cockpit-funnel || exit 1

# Resolve cargo/dolt BEFORE conf.sh, same hazard as test-lifecycle-container.sh: conf.sh
# can overwrite PATH with the harness's own tool directories.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin — needed to build spira-lc"
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — needed for spira_lifecycle's own throwaway server"

TMP="$(mktemp -d)"
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
BASE_PATH="$PATH"
BD_PATH="${SPIRA_PATH:-}"
REAL_BD="$(PATH="$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" command -v bd)"
[ -n "$REAL_BD" ] || { echo "SKIP cockpit-funnel: no bd binary" >&2; exit 77; }
TESTDB_BD_PATH="$(command -v bd)"
export PATH="$(dirname "$CARGO_BIN"):$(dirname "$DOLT_BIN"):$BASE_PATH"

# spira_lifecycle's own throwaway server — the same shape test-census.sh's own section 5
# and test-lc-hold.sh use. A distinct store on its own port, never dolt-beads.service.
LC_PORT=$((3309 + (RANDOM % 500)))
LC_SERVER_PID=""
mkdir -p "$TMP/lc-data"
cat > "$TMP/lc-server.yaml" <<YAML
log_level: warning
listener:
  port: $LC_PORT
  max_connections: 50
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$TMP/lc-data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
"$DOLT_BIN" sql-server --config "$TMP/lc-server.yaml" > "$TMP/lc-server.log" 2>&1 &
LC_SERVER_PID=$!
_lc_stop() { [ -n "$LC_SERVER_PID" ] && kill "$LC_SERVER_PID" >/dev/null 2>&1; }
trap '_lc_stop; testdb_drop; rm -rf "$TMP"' EXIT INT TERM

lc_up=0
for _ in $(seq 1 50); do
    if "$DOLT_BIN" --data-dir "$TMP/lc-data" --host 127.0.0.1 --port "$LC_PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        lc_up=1
        break
    fi
    sleep 0.2
done
[ "$lc_up" = 1 ] || bail "spira_lifecycle's throwaway dolt sql-server never came up: $(cat "$TMP/lc-server.log")"

REPO_ROOT="$(cd "$HERE/.." && pwd)"
CARGO_TARGET_DIR_FOR_LC="$TMP/lc-cargo-target"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_LC" \
    "$CARGO_BIN" build --manifest-path "$REPO_ROOT/spira-lc/Cargo.toml" --quiet 2>"$TMP/lc-build.log" \
    || bail "spira-lc failed to build: $(cat "$TMP/lc-build.log")"
LC_BIN="$CARGO_TARGET_DIR_FOR_LC/debug/spira-lc"

SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$LC_PORT" SPIRA_LC_DB=spira_lifecycle \
SPIRA_LC_DATA_DIR="$TMP/lc-data" SPIRA_LC_USER=root SPIRA_LC_PASSWORD="" \
SPIRA_LC_DOLT_BIN="$DOLT_BIN" \
    "$LC_BIN" admin-apply-ddl "$REPO_ROOT/lifecycle/schema.sql" >"$TMP/lc-schema.log" 2>&1
wantrc "spira_lifecycle schema applies cleanly" 0 $?

# lc_seed_bead <id> <state> <updated_at-epoch> [reason] -> a direct row insert, the same
# shape test-lifecycle-container.sh/test-census.sh's own seed_bead use.
lc_seed_bead() {
    local _reason_sql="NULL"
    [ -n "${4:-}" ] && _reason_sql="'$4'"
    "$DOLT_BIN" --data-dir "$TMP/lc-data" --host 127.0.0.1 --port "$LC_PORT" -u root -p "" --no-tls \
        --use-db spira_lifecycle sql -q \
        "INSERT INTO bead (bead_id, state, holds, reason, version, updated_at) VALUES ('$1','$2','[]',$_reason_sql,0,$3)
         ON DUPLICATE KEY UPDATE state='$2', reason=$_reason_sql, updated_at=$3" >/dev/null 2>&1
}

LC_ENV=(SPIRA_LC_BIN="$LC_BIN" SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$LC_PORT" \
        SPIRA_LC_DB=spira_lifecycle SPIRA_LC_DATA_DIR="$TMP/lc-data" \
        SPIRA_LC_USER=root SPIRA_LC_PASSWORD="" SPIRA_LC_DOLT_BIN="$DOLT_BIN")

REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" commit --allow-empty -m "init" -q

# done=3 (sp-d1..3), certify=2 (sp-g1..2), red=5 (sp-r1..5), certified=1 (sp-c1)
for br in sp-d1 sp-d2 sp-d3 sp-g1 sp-g2 sp-r1 sp-r2 sp-r3 sp-r4 sp-r5 sp-c1; do
    git -C "$REPO" checkout -q -b "spira/$br" main
    git -C "$REPO" commit --allow-empty -m "$br: work" -q
done
git -C "$REPO" checkout -q main

MAP="$TMP/repo-map"
printf '# name | path | land | base | format | gate\nalpha | %s | queue | main | |\n' "$REPO" > "$MAP"

RUN="$TMP/run"
mkdir -p "$RUN"
SPIRA_SCOPE_LABEL=alpha

AGO1H="$(date -u -d '65 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-65M +%Y-%m-%dT%H:%M:%SZ)"
AGO2H="$(date -u -d '2 hours ago'    +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-2H  +%Y-%m-%dT%H:%M:%SZ)"

for b in sp-d1 sp-d2 sp-d3 sp-g1 sp-g2 sp-r1 sp-r2 sp-r3 sp-r4 sp-r5 sp-c1; do
    printf '{"type":"system","subtype":"init"}\n' > "$RUN/$b.log"
done

testdb_seed <<JSONL
{"id":"sp-d1","title":"done bead 1","status":"closed","priority":2,"closed_at":"$AGO2H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-d2","title":"done bead 2","status":"closed","priority":2,"closed_at":"$AGO2H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-d3","title":"done bead 3","status":"closed","priority":2,"closed_at":"$AGO1H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-g1","title":"gated bead 1","status":"closed","priority":2,"closed_at":"$AGO1H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-g2","title":"gated bead 2","status":"closed","priority":2,"closed_at":"$AGO1H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-r1","title":"red timeout","status":"closed","priority":2,"closed_at":"$AGO1H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-r2","title":"red no-rebase","status":"closed","priority":2,"closed_at":"$AGO1H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-r3","title":"red gate","status":"closed","priority":2,"closed_at":"$AGO1H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-r4","title":"red conflict","status":"closed","priority":2,"closed_at":"$AGO1H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-r5","title":"red gate 2","status":"closed","priority":2,"closed_at":"$AGO1H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
{"id":"sp-c1","title":"certified bead","status":"closed","priority":2,"closed_at":"$AGO1H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
JSONL

NOW_EPOCH="$(date +%s)"
OLD_EPOCH=$(( NOW_EPOCH - 7200 ))  # 2h ago

# sp-g1..2: SUBMITTED (legacy GATED).
lc_seed_bead sp-g1 SUBMITTED "$OLD_EPOCH"
lc_seed_bead sp-g2 SUBMITTED "$NOW_EPOCH"

# sp-r1..5: REWORK (legacy RED), split by GateRedReason (lifecycle/src/reason.rs) —
# cockpit.sh's own bucket mapping is timeout->TIMEOUT, no-rebase->REBASE,
# {suites-failed,syntax,policy-violation}->GATE, confine->CONFLICT.
lc_seed_bead sp-r1 REWORK "$OLD_EPOCH" timeout
lc_seed_bead sp-r2 REWORK "$NOW_EPOCH" no-rebase
lc_seed_bead sp-r3 REWORK "$NOW_EPOCH" suites-failed
lc_seed_bead sp-r4 REWORK "$NOW_EPOCH" confine
lc_seed_bead sp-r5 REWORK "$NOW_EPOCH" policy-violation

# sp-c1: CERTIFIED.
lc_seed_bead sp-c1 CERTIFIED "$OLD_EPOCH"

# sp-d1..3: SUBMITTED too — the done stage is a handed-on bead awaiting certification with a
# branch and no landing (sp-mve9i; it was "closed in bd, no row", which cannot happen once
# the row is the bead's state), split by age at the cert window.
lc_seed_bead sp-d1 SUBMITTED "$NOW_EPOCH"
lc_seed_bead sp-d2 SUBMITTED "$NOW_EPOCH"
lc_seed_bead sp-d3 SUBMITTED "$NOW_EPOCH"

out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO=alpha SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_FAYTHS=t \
    SPIRA_PATH="$BD_PATH" \
        "${LC_ENV[@]}" \
    cockpit-collect once 2>/dev/null)"

val() { printf '%s' "$out" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

echo "--- (a) funnel stage counts ---"
# Done is every SUBMITTED bead with a branch: sp-d1..3 and sp-g1..2.
is "SP_UNLANDED_N is 5 (sp-d1..3, sp-g1..2: SUBMITTED)" "5"  "$(val SP_UNLANDED_N)"
# sp-bf31a: SP_UNLANDED_N splits into stranded (older than the cert window) and cert
# (within it). sp-d1/sp-d2 finished 2h ago are stranded; sp-d3, sp-g1, sp-g2 finished 65m
# ago are not (default window is 90m).
is "SP_STRANDED_N is 2 (sp-d1,sp-d2 closed 2h ago)" "2"  "$(val SP_STRANDED_N)"
is "SP_CERT_N is 3 (sp-d3, sp-g1..2 finished 65m ago)" "3"  "$(val SP_CERT_N)"
is "SP_FUNNEL_CERTIFY_N is 5 (sp-d1..3, sp-g1..2 SUBMITTED)" "5"  "$(val SP_FUNNEL_CERTIFY_N)"
is "SP_FUNNEL_RED_N is 5 (sp-r1..5)"          "5"  "$(val SP_FUNNEL_RED_N)"
is "SP_QUEUE_DEPTH is 1 (sp-c1 CERTIFIED)"    "1"  "$(val SP_QUEUE_DEPTH)"

echo "--- (a) red breakdown ---"
is "SP_FUNNEL_RED_TIMEOUT is 1"   "1" "$(val SP_FUNNEL_RED_TIMEOUT)"
is "SP_FUNNEL_RED_REBASE is 1"    "1" "$(val SP_FUNNEL_RED_REBASE)"
is "SP_FUNNEL_RED_GATE is 2"      "2" "$(val SP_FUNNEL_RED_GATE)"
is "SP_FUNNEL_RED_CONFLICT is 1"  "1" "$(val SP_FUNNEL_RED_CONFLICT)"

echo "--- (a) oldest ages: non-empty strings ---"
_certify_age="$(val SP_FUNNEL_CERTIFY_AGE)"
[ -n "$_certify_age" ] && [ "$_certify_age" != "?" ] \
    && ok "SP_FUNNEL_CERTIFY_AGE is non-empty (oldest SUBMITTED is 2h ago)" \
    || bad "SP_FUNNEL_CERTIFY_AGE is non-empty" "got [$_certify_age]"

_red_age="$(val SP_FUNNEL_RED_AGE)"
[ -n "$_red_age" ] && [ "$_red_age" != "?" ] \
    && ok "SP_FUNNEL_RED_AGE is non-empty (oldest REWORK is 2h ago)" \
    || bad "SP_FUNNEL_RED_AGE is non-empty" "got [$_red_age]"

_cert_age="$(val SP_FUNNEL_CERT_AGE)"
[ -n "$_cert_age" ] && [ "$_cert_age" != "?" ] \
    && ok "SP_FUNNEL_CERT_AGE is non-empty (CERTIFIED 2h ago)" \
    || bad "SP_FUNNEL_CERT_AGE is non-empty" "got [$_cert_age]"

_done_age="$(val SP_FUNNEL_DONE_AGE)"
[ -n "$_done_age" ] && [ "$_done_age" != "?" ] \
    && ok "SP_FUNNEL_DONE_AGE is non-empty (oldest done closed 2h ago)" \
    || bad "SP_FUNNEL_DONE_AGE is non-empty" "got [$_done_age]"

echo "--- (a) pane renders funnel ---"
PANE="health"
{
    printf '%s\n' "$out" | python3 -c '
import sys
for line in sys.stdin:
    line = line.rstrip("\n")
    if "=" not in line: continue
    k, _, v = line.partition("=")
    k = k.strip()
    if not k or not (k[0].isalpha() or k[0] == "_"): continue
    print("%s=%s" % (k, "\x27" + v.replace("\x27", "\x27\\\x27\x27") + "\x27"))
'
} > "$RUN/cockpit.env"

pane="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_FAYTHS=t \
    "$PANE" once 0 120 2>/dev/null)"

want "pane renders QUEUE label"    "QUEUE"   "$pane"
# sp-bf31a: the done row's count is the STRANDED anomaly (SP_STRANDED_N=2), not the raw
# SP_UNLANDED_N=3 — a bead still inside the cert window (sp-d3) is not yet actionable.
want "pane shows done row"         "done  2" "$pane"
want "pane shows certify row"      "certify" "$pane"
want "pane shows red row"          "red  5"  "$pane"
want "pane shows timeout in red"   "timeout" "$pane"
want "pane shows no-rebase in red" "no-rebase" "$pane"
want "pane shows gate in red"      "gate"    "$pane"
want "pane shows conflict in red"  "conflict" "$pane"
nowant "pane does not say anomaly" "anomaly" "$pane"

echo "--- (b) spira-lc unreachable ---"
# SPIRA_LC_BIN names a program that does not exist: CANNOT TELL renders "?" everywhere.
out_b="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO=alpha SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_FAYTHS=t \
    SPIRA_PATH="$BD_PATH" SPIRA_LC_BIN="$TMP/no-such-spira-lc" \
    cockpit-collect once 2>/dev/null)"

valb() { printf '%s' "$out_b" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

is "unreadable: SP_QUEUE_DEPTH=?"       "?" "$(valb SP_QUEUE_DEPTH)"
is "unreadable: SP_FUNNEL_CERTIFY_N=?"  "?" "$(valb SP_FUNNEL_CERTIFY_N)"
is "unreadable: SP_FUNNEL_RED_N=?"      "?" "$(valb SP_FUNNEL_RED_N)"
is "unreadable: SP_FUNNEL_CERT_AGE=?"   "?" "$(valb SP_FUNNEL_CERT_AGE)"

# Pane renders ? for funnel rows when spira-lc is unreachable.
{
    printf '%s\n' "$out_b" | python3 -c '
import sys
for line in sys.stdin:
    line = line.rstrip("\n")
    if "=" not in line: continue
    k, _, v = line.partition("=")
    k = k.strip()
    if not k or not (k[0].isalpha() or k[0] == "_"): continue
    print("%s=%s" % (k, "\x27" + v.replace("\x27", "\x27\\\x27\x27") + "\x27"))
'
} > "$RUN/cockpit.env"

pane_b="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_FAYTHS=t \
    "$PANE" once 0 120 2>/dev/null)"

want "unreadable: pane shows certify ?" "certify  ?" "$pane_b"
want "unreadable: pane shows red ?"     "red  ?"     "$pane_b"
nowant "unreadable: pane does not show certify 0" "certify  0" "$pane_b"
nowant "unreadable: pane does not show red 0"     "red  0"     "$pane_b"

echo "--- (c) positive control: SP_FUNNEL_* absent from env → ? in pane ---"
# When the collector env lacks funnel keys, the pane must not render 0 for them.
{
    printf 'SP_UNLANDED_N=3\n'
    printf 'SP_QUEUE_DEPTH=0\nSP_QUEUE_EJECTED=0\nSP_QUEUE_RED=0\n'
    printf 'SP_QUEUE_BATCH_PR=0\nSP_QUEUE_BATCH_N=0\nSP_QUEUE_NEXT_N=0\nSP_QUEUE_NEXT_MAX=8\nSP_QUEUE_QUARANTINE_N=0\n'
    # SP_FUNNEL_* intentionally omitted
} | python3 -c '
import sys
for line in sys.stdin:
    line = line.rstrip("\n")
    if "=" not in line: continue
    k, _, v = line.partition("=")
    k = k.strip()
    if not k or not (k[0].isalpha() or k[0] == "_"): continue
    print("%s=%s" % (k, "\x27" + v.replace("\x27", "\x27\\\x27\x27") + "\x27"))
' > "$RUN/cockpit.env"

pane_c="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_FAYTHS=t \
    "$PANE" once 0 120 2>/dev/null)"

nowant "absent keys: pane does not render certify 0" "certify  0" "$pane_c"
nowant "absent keys: pane does not render red 0"     "red  0"     "$pane_c"
want   "absent keys: pane shows certify ?"           "certify  ?" "$pane_c"
want   "absent keys: pane shows red ?"               "red  ?"     "$pane_c"

echo
tl_summary
