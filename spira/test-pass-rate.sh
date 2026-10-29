#!/usr/bin/env bash
# test-pass-rate.sh — passes-per-landing metric (gate-timing.sh rows) and its alarm (pass-rate.sh)
#
# Fixture rounds are real gate-timing.sh runs against result dirs, so the metric is read from
# the rows the harness writes, not from a hand-made ledger.
#
# tier: T0
# covers: spira/pass-rate.sh spira/gate-timing.sh spira/watchers spira/conf.d/SPIRA_PASS_RATE_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-pass-rate.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
SH="$TMP/spira"; mkdir -p "$SH"
cp -r "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null || true
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SH/"
ln -sf "$(command -v mail)" "$SH/mail"
chmod +x "$SH"/*.sh 2>/dev/null || true

RUN="$TMP/run"; mkdir -p "$RUN"
LEDGER="$TMP/ledger.tsv"

gate_row() {   # gate_row <branch> <verdict> [result-lines...] -> one gate-timing row
    local d="$TMP/res.$RANDOM$RANDOM" b="$1" v="$2"; shift 2
    mkdir -p "$d"
    printf 'key=%s\nbranch=%s\nbase=x\n' "$RANDOM" "$b" > "$d/batch.meta"
    local i=0 l
    for l in "$@"; do i=$((i+1)); printf '%s\n' "$l" > "$d/test-$i.result"; done
    SPIRA_BATCH_LEDGER="$LEDGER" SPIRA_RUN="$RUN" bash "$SH/gate-timing.sh" "$d" "$v"
}
col() { awk -F'\t' -v r="$1" -v c="$2" 'NR==r{print $c}' "$LEDGER"; }

pr() {
    tl_config SPIRA_HOME_REPO=x SPIRA_RUN="$RUN" SPIRA_MAIL="$RUN/mail" \
        SPIRA_MAIL_KINDS="$HERE/mail/kinds" SPIRA_MAIL_MUTE=0 SPIRA_MAIL_INDEX="$RUN/mail/index" \
        SPIRA_BATCH_LEDGER="$LEDGER" SPIRA_PASS_RATE_MAX_PCT=120 SPIRA_PASS_RATE_WINDOW=3600
    SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" bash "$SH/pass-rate.sh" "$@" 2>&1
}
mail_count() { ls "$RUN/mail/concierge/new" 2>/dev/null | wc -l | tr -d ' '; }
NOW="$(date +%s)"

pr check >/dev/null   # first call generates the conf defaults and prints about it
echo; echo "1 — rows carry over-cap, fault and passes-to-land"
gate_row r1 red 'ok 0 5 - p d 0' 'timeout 0 9 - p d 1' 'weird 0 3 - p d 1'
gate_row r1 red 'red 0 4 fp p d 1'
gate_row r1 green 'ok 0 5 - p d 0'
gate_row r2 green 'ok 0 5 - p d 0'
is "1: first pass over-cap count" "1" "$(col 1 16)"
is "1: first pass fault count"    "1" "$(col 1 17)"
is "1: first pass is pass 1"      "1" "$(col 1 18)"
is "1: second pass is pass 2"     "2" "$(col 2 18)"
is "1: green row carries passes-to-land" "3" "$(col 3 18)"
is "1: another branch counts from 1"     "1" "$(col 4 18)"

echo; echo "2 — 4 passes / 2 landings is 200% > 120%: over (positive control for the matcher)"
out="$(pr check --now "$NOW")"; rc=$?
is "2: exit 1" "1" "$rc"
is "2: counts [$out]" "4 2 200 over" "$out"

echo; echo "3 — a window that holds no pass is ok, and a ratio under the limit is ok"
out="$(pr check --now $((NOW + 7200)))"; rc=$?
is "3: empty window ok" "0 0 0 ok" "$out"
is "3: exit 0" "0" "$rc"
gate_row r3 green 'ok 0 5 - p d 0'
gate_row r4 green 'ok 0 5 - p d 0'
out="$(pr check --now "$NOW")"
is "3: 6 passes / 4 landings = 150% still over 120" "6 4 150 over" "$out"

echo; echo "4 — passes with no landing are over, not a divide by zero"
rm -f "$LEDGER"
gate_row r5 red 'red 0 4 fp p d 1'
is "4: no landing" "1 0 999999 over" "$(pr check --now "$NOW")"

echo; echo "5 — watch mails the crossing once and the clear once"
rm -f "$LEDGER" "$RUN/watchd/pass-rate.state"
gate_row r6 red 'red 0 4 fp p d 1'
pr watch --ticks 1 --interval 1 >/dev/null
is "5: over mailed" "1" "$(mail_count)"
pr watch --ticks 1 --interval 1 >/dev/null
is "5: no repeat while still over" "1" "$(mail_count)"
gate_row r6 green 'ok 0 5 - p d 0'
pr watch --ticks 1 --interval 1 >/dev/null
gate_row r7 green 'ok 0 5 - p d 0'
gate_row r8 green 'ok 0 5 - p d 0'
gate_row r9 green 'ok 0 5 - p d 0'
gate_row r10 green 'ok 0 5 - p d 0'
pr watch --ticks 1 --interval 1 >/dev/null
is "5: clear mailed once under the limit [$(pr check --now "$NOW")]" "2" "$(mail_count)"
pr health >/dev/null; is "5: health ok after a poll" "0" "$?"

echo; echo "6 — the watcher row and both keys exist"
grep -q '^pass-rate|daemon|@SPIRA_HOME@/pass-rate.sh watch' "$HERE/watchers"; is "6: watchers row" "0" "$?"
for k in SPIRA_PASS_RATE_MAX_PCT SPIRA_PASS_RATE_WINDOW; do [ -f "$HERE/conf.d/$k" ]; is "6: $k registered" "0" "$?"; done

tl_summary
