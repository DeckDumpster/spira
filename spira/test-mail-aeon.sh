#!/usr/bin/env bash
#
# test-mail-aeon.sh — aeon mail delivery: per-claim mailboxes and the PostToolUse hook.
#
# Acceptance criteria (each seen red first):
#   (a) message dropped into the aeon's new/ appears in the hook's additionalContext exactly once
#   (b) bead.sh amend on an in_progress bead with a live aeon mails the change
#   (c) amend on an open bead sends nothing and succeeds
#   (d) mailbox gone after the aeon exits
#   (e) empty-mailbox hook adds no context
#
# (a), (e): hook tested directly; no agent session needed.
# (b), (c): bead.sh amend tested with real db and a fake pidfile (live = bash $$).
# (d): aeon.sh run with a stub agent that immediately closes the bead.
#
# covers: spira/hooks/aeon-mail-deliver.sh spira/bead.sh spira/mail.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isz()    { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz()   { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero got 0"; }
has()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
hasnt()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

. "$HERE/testdb.sh"
testdb_require test-mail-aeon
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up mailaeon || { echo "test-mail-aeon: could not build fixture db"; exit 1; }
# bdq is a lib.sh function; define a thin wrapper so test-level calls reach the fixture db.
bdq() { BD_IGNORE_SCHEMA_SKEW=1 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" "$@"; }

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber" "$SPIRA_HOME/hooks"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=""   # prevent reading a real spira.conf

cp "$HERE/mail.sh" "$SPIRA_HOME/"
cp "$HERE/hooks/aeon-mail-deliver.sh" "$SPIRA_HOME/hooks/"
HOOK="$SPIRA_HOME/hooks/aeon-mail-deliver.sh"

run_mail() { SPIRA_HOME="$SPIRA_HOME" bash "$SPIRA_HOME/mail.sh" "$@"; }

# ==========================================================================
# (e) SEEN RED: empty mailbox hook must have been able to find something — plant
#     a message, verify it appears, then check silence after moving to cur.
# (a) + (e): hook output with and without messages.
# ==========================================================================
echo
echo "hook — positive control (SEEN RED then SEEN GREEN)"

BID="test-bead-$$"
mkdir -p "$SPIRA_MAIL/aeon-$BID/new" "$SPIRA_MAIL/aeon-$BID/cur" "$SPIRA_MAIL/aeon-$BID/tmp"

# Plant an offender
MSGFILE="$SPIRA_MAIL/aeon-$BID/new/123.msg"
printf 'From: Operator <op@spira>\nSubject: Scope change\nDate: Mon, 1 Jan 2024 00:00:00 +0000\nMessage-ID: <123@spira>\n\nNew requirement added.\n' > "$MSGFILE"

# SEEN RED: hook with a message — must output additionalContext
hook_out="$(SPIRA_MAIL="$SPIRA_MAIL" BEAD_ID="$BID" bash "$HOOK" </dev/null 2>&1)"; rc=$?
isz  "SEEN RED (a): hook exits 0 with a message"  "$rc"
has  "SEEN RED (a): output contains additionalContext"  "additionalContext"  "$hook_out"
has  "SEEN RED (a): output contains the message body"   "New requirement"    "$hook_out"
has  "SEEN RED (a): output contains the sender"         "Operator"           "$hook_out"

# Message moved to cur/
cur_count="$(ls "$SPIRA_MAIL/aeon-$BID/cur" 2>/dev/null | wc -l | tr -d ' ')"
is "SEEN RED (a): message moved to cur/"  "1"  "$cur_count"
new_count="$(ls "$SPIRA_MAIL/aeon-$BID/new" 2>/dev/null | wc -l | tr -d ' ')"
is "SEEN RED (a): new/ empty after delivery"  "0"  "$new_count"

echo
echo "hook — empty mailbox (SEEN GREEN for criterion e)"

# SEEN GREEN (e): no messages — hook must produce no output
hook_empty="$(SPIRA_MAIL="$SPIRA_MAIL" BEAD_ID="$BID" bash "$HOOK" </dev/null 2>&1)"; rc=$?
isz    "SEEN GREEN (e): hook exits 0 on empty mailbox"      "$rc"
is     "SEEN GREEN (e): hook produces no output when empty" "" "$hook_empty"

echo
echo "hook — message delivered exactly once (SEEN GREEN for criterion a)"

# Plant another message and verify it appears only once
printf 'From: Ops <ops@spira>\nSubject: Reminder\nDate: Mon, 1 Jan 2024 00:01:00 +0000\nMessage-ID: <456@spira>\n\nDo not forget to rebase.\n' \
    > "$SPIRA_MAIL/aeon-$BID/new/456.msg"
hook_once="$(SPIRA_MAIL="$SPIRA_MAIL" BEAD_ID="$BID" bash "$HOOK" </dev/null 2>&1)"
has    "SEEN GREEN (a): message appears in output"  "Do not forget"  "$hook_once"
# Run again — should be empty now
hook_second="$(SPIRA_MAIL="$SPIRA_MAIL" BEAD_ID="$BID" bash "$HOOK" </dev/null 2>&1)"
is     "SEEN GREEN (a): message does not appear twice"  ""  "$hook_second"

echo
echo "hook — no BEAD_ID set → no output"

hook_nobid="$(SPIRA_MAIL="$SPIRA_MAIL" BEAD_ID="" bash "$HOOK" </dev/null 2>&1)"; rc=$?
isz  "hook exits 0 when BEAD_ID is empty"   "$rc"
is   "hook is silent when BEAD_ID is empty" "" "$hook_nobid"

# ==========================================================================
# mail.sh aeon: address routing
# ==========================================================================
echo
echo "mail.sh aeon: address routing"

ALIVE_BID="alive-bead-$$"
mkdir -p "$SPIRA_MAIL/aeon-$ALIVE_BID/new" "$SPIRA_MAIL/aeon-$ALIVE_BID/cur" "$SPIRA_MAIL/aeon-$ALIVE_BID/tmp"

# SEEN RED: aeon:<id> with live mailbox should deliver
out="$(echo "Test body." | SPIRA_MAIL="$SPIRA_MAIL" run_mail send "aeon:$ALIVE_BID" \
    --from "Test <t@t>" --subject "Hello" 2>&1)"; rc=$?
isz  "SEEN RED: aeon:<id> with live mailbox delivers"  "$rc"
cnt="$(ls "$SPIRA_MAIL/aeon-$ALIVE_BID/new" 2>/dev/null | wc -l | tr -d ' ')"
is   "SEEN RED: message lands in aeon mailbox"  "1"  "$cnt"

# SEEN GREEN: aeon:<id> with no mailbox should refuse
DEAD_BID="dead-bead-$$"
out2="$(echo "body" | SPIRA_MAIL="$SPIRA_MAIL" run_mail send "aeon:$DEAD_BID" \
    --from "Test <t@t>" --subject "Hello" 2>&1)"; rc2=$?
isnz "SEEN GREEN: aeon:<id> with no mailbox exits non-zero"   "$rc2"
has  "SEEN GREEN: refusal mentions aeon"  "aeon"  "$out2"

# ==========================================================================
# bead.sh amend — (b) and (c)
# ==========================================================================
echo
echo "bead.sh amend — in_progress bead with live aeon (b)"

export SPIRA_REPO_MAP="$TMP/repo-map"
printf '' > "$SPIRA_REPO_MAP"   # empty; amend does not need it

BEAD_HOME_ORIG="$HERE"
run_bead() { SPIRA_HOME="$SPIRA_HOME" SPIRA_MAIL="$SPIRA_MAIL" SPIRA_RUN="$SPIRA_RUN" \
             bash "$HERE/bead.sh" "$@"; }

# File a bead in the fixture db, then claim it manually.
BID2="$(bdq create "Test amend bead" -l "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan},repo:fixture" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d if isinstance(d,dict) else d[0])["id"])' 2>/dev/null)"
[ -n "$BID2" ] || { bad "bead.sh amend: could not file test bead" ""; echo "$pass passed, $fail failed"; exit 1; }

bdq update "$BID2" --claim >/dev/null 2>&1 || true

# Fake the pidfile so aeon_alive() returns true (bash itself is the "aeon").
FAKE_PID=$$
FAKE_PF="$SPIRA_RUN/aeon-builder-$BID2.pid"
echo "$FAKE_PID" > "$FAKE_PF"

# Create the aeon's mailbox.
mkdir -p "$SPIRA_MAIL/aeon-$BID2/new" "$SPIRA_MAIL/aeon-$BID2/cur" "$SPIRA_MAIL/aeon-$BID2/tmp"

# SEEN RED: amend with live aeon → mail is delivered
amend_rc=0
run_bead amend "$BID2" --note "Scope expanded: add acceptance tests" >/dev/null 2>&1 || amend_rc=$?
isz  "SEEN RED (b): amend exits 0 with live aeon"  "$amend_rc"
mbx_new="$(ls "$SPIRA_MAIL/aeon-$BID2/new" 2>/dev/null | wc -l | tr -d ' ')"
is   "SEEN RED (b): amend delivers mail to aeon mailbox"  "1"  "$mbx_new"

# Read the mail and verify it contains the change
msg_body="$(cat "$SPIRA_MAIL/aeon-$BID2/new"/* 2>/dev/null)"
has  "SEEN RED (b): mail contains the note text"  "Scope expanded"  "$msg_body"

rm -f "$FAKE_PF"
rm -rf "$SPIRA_MAIL/aeon-$BID2"

echo
echo "bead.sh amend — open bead with no live aeon (c)"

BID3="$(bdq create "Test no-aeon bead" -l "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan},repo:fixture" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d if isinstance(d,dict) else d[0])["id"])' 2>/dev/null)"
[ -n "$BID3" ] || { bad "bead.sh amend (c): could not file test bead" ""; echo "$pass passed, $fail failed"; exit 1; }

# SEEN GREEN: amend with no live aeon → succeeds, no mail
amend3_rc=0
run_bead amend "$BID3" --note "No aeon watching" >/dev/null 2>&1 || amend3_rc=$?
isz    "SEEN GREEN (c): amend exits 0 with no live aeon"   "$amend3_rc"
is     "SEEN GREEN (c): no mailbox created"  "" \
    "$(ls "$SPIRA_MAIL/aeon-$BID3/new" 2>/dev/null | tr '\n' ' ' | tr -d ' ')"

# ==========================================================================
# (d) mailbox removed after aeon exits
# ==========================================================================
# Close leftover open/in_progress beads from (b) and (c) so aeon.sh claims BID4 only.
bdq close "$BID2" --reason "test cleanup" >/dev/null 2>&1 || true
bdq close "$BID3" --reason "test cleanup" >/dev/null 2>&1 || true

echo
echo "(d) mailbox gone after aeon exits"

# Set up a minimal aeon environment with a stub that immediately closes the bead.
AEON_ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$AEON_ORIGIN"
AEON_REPO="$TMP/repo"; git clone -q "$AEON_ORIGIN" "$AEON_REPO" 2>/dev/null
git -C "$AEON_REPO" config user.email t@t; git -C "$AEON_REPO" config user.name t
printf 'seed\n' > "$AEON_REPO/f"
git -C "$AEON_REPO" add f
git -C "$AEON_REPO" commit -qm seed
git -C "$AEON_REPO" push -q origin main 2>/dev/null

printf 'fixture | %s | push | origin/main | |\n' "$AEON_REPO" > "$SPIRA_HOME/repo-map"
export SPIRA_REPO_MAP="$SPIRA_HOME/repo-map"

cat > "$SPIRA_HOME/chamber/builder.fayth" <<'FAYTH'
FAYTH_NAME=builder
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

# Stub agent: commits the bead id and closes the bead, then exits.
BIN="$TMP/bin"; mkdir -p "$BIN"
# Guard: refuse to run the real agent.
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { bad "(d): aeon.sh has no SPIRA_AGENT injection point" ""; echo "$pass passed, $fail failed"; exit 1; }

cat > "$BIN/claude" <<'STUB'
#!/usr/bin/env bash
cat >/dev/null  # drain prompt
id="$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
[ -n "$id" ] || exit 1
printf 'work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id: done" >/dev/null 2>&1
BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":100,"num_turns":1,"total_cost_usd":0}\n'
STUB
chmod +x "$BIN/claude"

BID4="$(bdq create "Test mailbox cleanup bead" -l "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan},repo:fixture" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d if isinstance(d,dict) else d[0])["id"])' 2>/dev/null)"
[ -n "$BID4" ] || { bad "(d): could not file test bead" ""; echo "$pass passed, $fail failed"; exit 1; }

# Run aeon; it should create the mailbox and then remove it.
aeon_rc=0
SPIRA_HOME="$SPIRA_HOME" SPIRA_RUN="$SPIRA_RUN" SPIRA_MAIL="$SPIRA_MAIL" \
SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-bd}" \
SPIRA_AGENT="$BIN/claude" SPIRA_CONF="" \
GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
    bash "$HERE/aeon.sh" builder >/dev/null 2>&1 || aeon_rc=$?

# SEEN RED: the mailbox was created (and BID4 was in_progress); SEEN GREEN: gone after exit.
# We verify absence — the positive control is that aeon.sh creates it (observed indirectly
# through the fact that the aeon ran and the bead is now closed).
status="$(bdq show "$BID4" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,dict) else d[0]; print(d.get("status",""))' 2>/dev/null)"
is "SEEN RED (d): bead was closed by the stub aeon"  "closed"  "$status"

mbx_path="$SPIRA_MAIL/aeon-$BID4"
if [ -d "$mbx_path" ]; then
    bad "(d): mailbox still present after aeon exited" "$mbx_path"
else
    ok "(d): mailbox gone after aeon exited"
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
