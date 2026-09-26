#!/usr/bin/env bash
#
# test-cockpit-funnel.sh — DONE→LANDED pipeline funnel in the QUEUE section.
#
# Three scenarios:
#   (a) fixture with the exact landstate shapes from the bead description:
#       done=3 (no landstate), certify=2 (GATED), red=5 (timeout/rebase/gate/conflict),
#       certified=1 (CERTIFIED). Verifies counts, breakdown, oldest ages, pane rendering.
#   (b) unreadable landstate directory — all funnel keys must be ?, pane shows ?.
#   (c) positive control — funnel keys absent from env renders ? in the pane (not 0).
#
# tier: T2
# covers: spira/cockpit.sh cockpit/health.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"
testdb_require cockpit-funnel
testdb_up cockpit-funnel || exit 1


TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
BASE_PATH="$PATH"
BD_PATH="${SPIRA_PATH:-}"
REAL_BD="$(PATH="$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" command -v bd)"
[ -n "$REAL_BD" ] || { echo "SKIP cockpit-funnel: no bd binary" >&2; exit 77; }
TESTDB_BD_PATH="$(command -v bd)"

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

mkdir -p "$RUN/landstate"

TIP_G1="$(git -C "$REPO" rev-parse spira/sp-g1)"
TIP_G2="$(git -C "$REPO" rev-parse spira/sp-g2)"
printf 'GATED %s %s pass:running\n' "$TIP_G1" "$OLD_EPOCH" > "$RUN/landstate/sp-g1"
printf 'GATED %s %s pass:running\n' "$TIP_G2" "$NOW_EPOCH" > "$RUN/landstate/sp-g2"

TIP_R1="$(git -C "$REPO" rev-parse spira/sp-r1)"
TIP_R2="$(git -C "$REPO" rev-parse spira/sp-r2)"
TIP_R3="$(git -C "$REPO" rev-parse spira/sp-r3)"
TIP_R4="$(git -C "$REPO" rev-parse spira/sp-r4)"
TIP_R5="$(git -C "$REPO" rev-parse spira/sp-r5)"
printf 'RED %s %s timeout\n'               "$TIP_R1" "$OLD_EPOCH" > "$RUN/landstate/sp-r1"
printf 'RED %s %s no-rebase@deadbeef\n'    "$TIP_R2" "$NOW_EPOCH" > "$RUN/landstate/sp-r2"
printf 'RED %s %s gate\n'                  "$TIP_R3" "$NOW_EPOCH" > "$RUN/landstate/sp-r3"
printf 'RED %s %s conflicts-with-base\n'   "$TIP_R4" "$NOW_EPOCH" > "$RUN/landstate/sp-r4"
printf 'RED %s %s gate\n'                  "$TIP_R5" "$NOW_EPOCH" > "$RUN/landstate/sp-r5"

TIP_C1="$(git -C "$REPO" rev-parse spira/sp-c1)"
printf 'CERTIFIED %s %s\n' "$TIP_C1" "$OLD_EPOCH" > "$RUN/landstate/sp-c1"

# sp-d1..3 have no landstate (done stage).

out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO=alpha SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_PATH="$BD_PATH" \
    bash "$HERE/cockpit.sh" once 2>/dev/null)"

val() { printf '%s' "$out" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

echo "--- (a) funnel stage counts ---"
is "SP_UNLANDED_N is 3 (sp-d1..3: done)"     "3"  "$(val SP_UNLANDED_N)"
# sp-bf31a: SP_UNLANDED_N splits into stranded (older than the cert window) and cert
# (within it). sp-d1/sp-d2 closed 2h ago are stranded; sp-d3 closed 65m ago is not
# (default window is 90m).
is "SP_STRANDED_N is 2 (sp-d1,sp-d2 closed 2h ago)" "2"  "$(val SP_STRANDED_N)"
is "SP_CERT_N is 1 (sp-d3 closed 65m ago)"          "1"  "$(val SP_CERT_N)"
is "SP_FUNNEL_CERTIFY_N is 2 (sp-g1..2)"      "2"  "$(val SP_FUNNEL_CERTIFY_N)"
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
    && ok "SP_FUNNEL_CERTIFY_AGE is non-empty (oldest GATED is 2h ago)" \
    || bad "SP_FUNNEL_CERTIFY_AGE is non-empty" "got [$_certify_age]"

_red_age="$(val SP_FUNNEL_RED_AGE)"
[ -n "$_red_age" ] && [ "$_red_age" != "?" ] \
    && ok "SP_FUNNEL_RED_AGE is non-empty (oldest RED is 2h ago)" \
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
PANE="$HERE/../cockpit/health.sh"
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
    bash "$PANE" once 0 120 2>/dev/null)"

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

echo "--- (b) unreadable landstate directory ---"
# Remove the landstate dir entirely — all funnel keys must be ?.
rm -rf "$RUN/landstate"

out_b="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO=alpha SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_PATH="$BD_PATH" \
    bash "$HERE/cockpit.sh" once 2>/dev/null)"

valb() { printf '%s' "$out_b" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

is "unreadable: SP_QUEUE_DEPTH=?"       "?" "$(valb SP_QUEUE_DEPTH)"
is "unreadable: SP_FUNNEL_CERTIFY_N=?"  "?" "$(valb SP_FUNNEL_CERTIFY_N)"
is "unreadable: SP_FUNNEL_RED_N=?"      "?" "$(valb SP_FUNNEL_RED_N)"
is "unreadable: SP_FUNNEL_CERT_AGE=?"   "?" "$(valb SP_FUNNEL_CERT_AGE)"

# Pane renders ? for funnel rows when landstate is unreadable.
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
    bash "$PANE" once 0 120 2>/dev/null)"

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
    bash "$PANE" once 0 120 2>/dev/null)"

nowant "absent keys: pane does not render certify 0" "certify  0" "$pane_c"
nowant "absent keys: pane does not render red 0"     "red  0"     "$pane_c"
want   "absent keys: pane shows certify ?"           "certify  ?" "$pane_c"
want   "absent keys: pane shows red ?"               "red  ?"     "$pane_c"

echo
tl_summary
