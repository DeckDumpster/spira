#!/usr/bin/env bash
#
# test-session-hook.sh — the session hook's output, and its registration in the client's
# own settings file.
#
#   ./test-session-hook.sh
#
# WHAT IT HOLDS. The hook's output is prepended to a context window that has just opened, and
# its registration lives in a file no landing gate can reach. Both fail silently by nature, so
# every property below is one that would otherwise be believed rather than known:
#
#   1. A SUMMARY, NEVER A REPLAY. A three-hundred-event backlog must cost a glance. The
#      assertion is on the line count against the configured budget, because the failure being
#      fixed was a hook offering to replay 283 raw lines into a fresh session.
#   2. IT MARKS NOTHING READ. The hook peeks. If it drained, every line it had no room for
#      would be recorded as delivered and the latch that follows would replay nothing — data
#      loss that grows with the size of the backlog.
#   3. DEGRADED IS LIFTED OUT OF THE TABLE. A watcher that is running and blind is silent in
#      exactly the way a healthy quiet one is, so the word is surfaced in its own section —
#      and the suite plants one before believing that it can be absent
#      (law-absence-needs-a-positive-control).
#   4. THE LATCH COMMAND IS THERE FOR EVERY ROW. A hook cannot attach a Monitor, so the
#      command it prints is the only path back to the events it summarised.
#   5. IT NEVER BREAKS A SESSION START. No stdin, malformed stdin, no manifest, an unreadable
#      one: exit 0 every time, and silence where there is nothing to say.
#   6. THE REGISTRATION IS MANAGED, NOT DESCRIBED. Installing twice leaves one entry,
#      unrelated settings survive, and the entry carries NO matcher — a SessionStart matcher
#      naming a subset of sources is how a hook comes to be missing from `compact` and `fork`.
#
# It needs no database and no beads server. Every configured value is pinned to a NON-DEFAULT,
# so a literal written into the hook cannot pass by coincidence, and nothing here can reach
# the operator's own configuration or their live client settings file.
#
# defect: sp-4vp
# tier: T1
# covers: spira/install-session-hook.sh spira/hooks/session.sh spira/watchd.sh spira/mail.sh systemd/install.sh systemd/cockpit-ensure.service
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "$2" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1" "$2" ;; *) ok "$1" ;; esac; }
le()  { if [ "$3" -le "$2" ]; then ok "$1"; else bad "$1" "want <= $2, got $3"; fi; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home" "$TMP/bin"

# A harness tree that is NOT this checkout, so nothing here can read the operator's own
# configuration, their watcher manifest or their client settings and report a pass it did not
# earn.
CLONE="$TMP/clone"
mkdir -p "$CLONE/spira/hooks"
cp "$HERE/conf.sh" "$HERE/watchd.sh" "$HERE/install-session-hook.sh" "$HERE/mail.sh" "$CLONE/spira/"
cp "$HERE/hooks/session.sh" "$CLONE/spira/hooks/"

# `status` asks systemd about every daemon row. A stub answers instead, so this suite says
# nothing about whether the box it runs on has a user manager.
cat > "$TMP/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
for a in "$@"; do case "$a" in spira-watch@*) echo active ;; esac; done
exit 0
EOF
chmod +x "$TMP/bin/systemctl"

# EVERY CONFIGURED VALUE PINNED TO A NON-DEFAULT. SPIRA_RUN would derive to $CLONE/.runtime,
# SPIRA_WATCHERS to $CLONE/spira/watchers and the budget to the shipped number; all three are
# moved, so a literal written into the hook cannot pass.
RUN="$TMP/elsewhere/run"; mkdir -p "$RUN/watchd"
MANIFEST="$TMP/elsewhere/watchers"
BUDGET=26
MAIL_DIR="$TMP/elsewhere/mail"
mkdir -p "$MAIL_DIR/concierge/new" "$MAIL_DIR/concierge/cur" "$MAIL_DIR/concierge/tmp"
CONF="$TMP/spira.conf"
cat > "$CONF" <<EOF
# THE FIXTURE DECLARES ITSELF IN FORCE. The hook refuses to print from a harness that is not
# the one systemd runs, so a clone that left SPIRA_PROD at its derived default would be silent
# here and every assertion below would pass on an empty string.
SPIRA_PROD = $CLONE/spira
SPIRA_RUN = $RUN
SPIRA_WATCHERS = $MANIFEST
SPIRA_HOOK_LINES = $BUDGET
SPIRA_CLIENT_SETTINGS = $TMP/elsewhere/settings.json
SPIRA_MAIL = $MAIL_DIR
SPIRA_MAIL_SESSION_MAILBOX = concierge
EOF

cat > "$MANIFEST" <<'EOF'
answers|daemon|/bin/sleep 3600
cron|log|@SPIRA_RUN@/watchd/somebody-elses.log
EOF

# THE BACKLOG. Three hundred events, thirty of them actionable — the same shape as the real
# one that prompted this: mostly progress, a minority that needs somebody.
python3 - "$RUN/watchd/answers.log" <<'PY'
import sys
with open(sys.argv[1], "w") as fh:
    for i in range(300):
        if i % 10 == 0:
            fh.write("ESCALATED sp-%03d wants a decision\n" % i)
        else:
            fh.write("progress %d, and nothing to do about it\n" % i)
PY
: > "$RUN/watchd/somebody-elses.log"

# hook <event> <source> [env...] — run the hook exactly as the client does: the payload on
# stdin, in a minimal environment that cannot reach the operator's configuration.
hook() {
    local ev="$1" src="$2"; shift 2
    printf '{"hook_event_name":"%s","source":"%s"}' "$ev" "$src" \
      | env -i HOME="$TMP/home" PATH="$TMP/bin:$PATH" SPIRA_CONF="$CONF" SPIRA_WAKE="${WAKE-}" "$@" \
        bash "$CLONE/spira/hooks/session.sh"
}

echo "a summary, never a replay — the positive control"
# A CHECK THAT FINDS NOTHING MUST FIRST PROVE IT COULD HAVE FOUND SOMETHING. Every absence
# asserted below is believable only because this passes: the hook can read a real manifest and
# a real backlog, so silence later is about the case and not about the fixture.
out="$(hook SessionStart startup)"; rc=$?
is  "the hook exits clean"                       "0" "$rc"
has "it names itself"                            "$out" "## Spira watchers"
has "the status table is there whole"            "$out" "answers"
has "and so is the log row"                      "$out" "cron"
has "the backlog's size is stated"               "$out" "300"
has "and how much of it was actionable"          "$out" "30 actionable of 300 new"
n="$(printf '%s\n' "$out" | wc -l)"
# THE ACCEPTANCE CRITERION, and it is measured rather than estimated: the hook assembles the
# fixed sections first and gives the preview exactly what is left.
le  "a 300-event backlog fits the budget"        "$BUDGET" "$n"
[ "$n" -gt 8 ] && ok "and it is not empty — $n lines" || bad "and it is not empty" "$n lines"

echo
echo "and it marks nothing read"
# IF THE HOOK DRAINED, the lines it had no room for would be recorded as delivered and the
# latch below would replay nothing. The cursor is the whole evidence: the contract is a log
# and an integer, and the integer must not have moved.
is  "no cursor file is written"                  "" "$(ls "$RUN/watchd" | grep cursor || true)"
after="$(env -i HOME="$TMP/home" PATH="$TMP/bin:$PATH" SPIRA_CONF="$CONF" \
         bash "$CLONE/spira/watchd.sh" status)"
has "so the backlog is still unread afterwards"  "$after" "300"
out2="$(hook SessionStart clear)"
has "and a second session sees the same 300"     "$out2" "30 actionable of 300 new"

echo
echo "no Monitor instructions are printed"
# THE HOOK PRINTS NO LATCH COMMANDS. Delivery and replies now handle operator communication
# through mail; the Monitor latch is retired.
hasnt "no Monitor instruction is emitted"         "$out" "Monitor:"
hasnt "no resume-from-cursor text"                "$out" "Nothing above was marked read"
# IT NEVER SENDS THE READER TO ListAgents. ListAgents enumerates agents and sessions and no
# tool enumerates a session's own Monitors, so "attach only the streams not already listed
# there" reported nothing attached every time (sp-vv4p, superseded).
hasnt "it does not send the reader to ListAgents" "$out" "ListAgents"

echo
echo "the newest lines are the ones kept"
# `peek` KEEPS EACH WATCHER'S MOST RECENT LINES. A backlog is read for its current state, so a
# cap that kept the oldest would answer the least useful question — and an early version did
# exactly that by trimming the assembled text from the end.
has "the newest actionable line survives the cap" "$out" "sp-290"
hasnt "and the oldest is the one withheld"        "$out" "sp-000"
has "with the withholding stated, not silent"     "$out" "withheld"

echo
echo "the budget is a ceiling, and it is configuration"
for b in 20 60; do
    m="$(hook SessionStart startup SPIRA_HOOK_LINES="$b" | wc -l)"
    le "a budget of $b is honoured" "$b" "$m"
done
# AND A LITERAL COULD NOT PASS EITHER OF THOSE, because a wider budget must actually print
# more: a hook that ignored the key would give the same number twice.
a="$(hook SessionStart startup SPIRA_HOOK_LINES=20 | wc -l)"
b="$(hook SessionStart startup SPIRA_HOOK_LINES=60 | wc -l)"
if [ "$b" -gt "$a" ]; then ok "a wider budget prints more ($a then $b)"
else bad "a wider budget prints more" "$a then $b"; fi

echo
echo "every session-start source is summarised, none is special"
# A SessionStart carries one of five sources and every one of them opens a context window with
# no Monitor attached. `compact` and `fork` are the two a hand-written matcher omits, so they
# are the two asserted here.
for src in startup resume clear compact fork; do
    got="$(hook SessionStart "$src")"
    has "source '$src' is summarised" "$got" "## Spira watchers"
done
# PostCompact carries a trigger rather than a source, and an automatic compaction is precisely
# the one nobody is present for.
has "PostCompact is summarised too" "$(hook PostCompact auto)" "## Spira watchers"

echo
echo "SessionEnd has nothing to say"
# Its output would go into the context that is being discarded, and under systemd there are no
# processes for a departing session to guarantee.
out3="$(hook SessionEnd clear)"; rc=$?
is "SessionEnd exits clean"   "0" "$rc"
is "and prints nothing at all" "" "$out3"

echo
echo "it never breaks a session start"
run_raw() {                        # run_raw <stdin> — the hook with an arbitrary payload
    printf '%s' "$1" | env -i HOME="$TMP/home" PATH="$TMP/bin:$PATH" SPIRA_CONF="$CONF" \
        bash "$CLONE/spira/hooks/session.sh"
}
out4="$(run_raw 'not json at all')"; is "malformed stdin still exits clean" "0" "$?"
has "and still summarises"        "$out4" "## Spira watchers"
out5="$(run_raw '')";              is "empty stdin still exits clean"      "0" "$?"
has "and still summarises"        "$out5" "## Spira watchers"

# NO WATCHERS MEANS NO OUTPUT. This hook is registered in the client's own settings, so it
# runs in every session on the box whatever repository that session is in. A banner in each of
# them for a thing the operator does not use is the exact noise this replaced.
EMPTY="$TMP/elsewhere/empty-manifest"; : > "$EMPTY"
out6="$(hook SessionStart startup SPIRA_WATCHERS="$EMPTY")"; rc=$?
is "an empty manifest exits clean"   "0" "$rc"
is "and says nothing at all"         "" "$out6"
out7="$(hook SessionStart startup SPIRA_WATCHERS="$TMP/elsewhere/not-a-file")"; rc=$?
is "an absent manifest exits clean"  "0" "$rc"
is "and says nothing at all"         "" "$out7"
# A MANIFEST THAT DOES NOT PARSE IS THE SAME. watchd refuses the whole file and names the
# fault on stderr; the hook has nothing to report and must not report half of it.
BROKEN="$TMP/elsewhere/broken-manifest"; printf 'this is not a row\n' > "$BROKEN"
out8="$(hook SessionStart startup SPIRA_WATCHERS="$BROKEN" 2>/dev/null)"; rc=$?
is "a malformed manifest exits clean" "0" "$rc"
is "and says nothing at all"          "" "$out8"

echo
echo "DEGRADED is lifted out of the table"
# DRIVEN THROUGH THE REAL `watchd.sh`, never a planted table (law-prefer-the-real-dependency).
# An earlier version of this section stubbed `status` with the table's shape as it was
# remembered, and that stub is precisely what let a real defect through: `status` grew a
# trailing `DEGRADED` block below the table, the stub did not, and the hook — which read the
# table as "everything after line one" — swallowed the block's lines as rows and printed
# `watchd.sh tail DEGRADED` as a latch command. The suite was green throughout. A health probe
# that exits non-zero is all the real thing needs, so there is nothing here worth faking.
DRUN="$TMP/elsewhere/drun"; mkdir -p "$DRUN/watchd"
printf 'one event\n' > "$DRUN/watchd/answers.log"
printf 'one event\n' > "$DRUN/watchd/cron.log"

# Two manifests differing ONLY in whether the probe succeeds, so the healthy case below is a
# genuine control over the same code path rather than a different fixture.
dmanifest() {
    cat > "$1" <<EOF
answers|log|$DRUN/watchd/answers.log|$2
cron|log|$DRUN/watchd/cron.log|true
EOF
}
DBAD="$TMP/elsewhere/watchers.degraded"; DOK="$TMP/elsewhere/watchers.ok"
dmanifest "$DBAD" 'echo "no local ids in the state file" >&2; exit 1'
dmanifest "$DOK"  'true'

dhook() { hook SessionStart startup SPIRA_RUN="$DRUN" SPIRA_WATCHERS="$1"; }

dout="$(dhook "$DBAD")"
has "a DEGRADED watcher gets its own section" "$dout" "DEGRADED — running and blind"
has "silence from it is called out"           "$dout" "not good news"
# THE REASON, NOT JUST THE WORD. `DEGRADED` alone says a check failed and not which, so the
# next step would be to re-run the probe by hand — the work this output exists to have done.
has "and it carries the probe's own reason"   "$dout" "no local ids in the state file"
has "attributed to the watcher that earned it" "$dout" "answers:"

hasnt "no Monitor instruction in DEGRADED case"   "$dout" "Monitor:"

# AND THE SECTION APPEARS ONCE. Printing the status whole and then adding a call-out rendered
# the same reason twice, which reads as two faults.
is  "the reason is printed exactly once"      "1" \
    "$(printf '%s\n' "$dout" | grep -c 'no local ids in the state file' || true)"

hout="$(dhook "$DOK")"
hasnt "a healthy manifest raises no such section" "$hout" "running and blind"
has  "but the table is still printed"             "$hout" "answers"
hasnt "no Monitor instruction in healthy case"    "$hout" "Monitor:"

echo
echo "a watcher this installation has not got is shown, and never latched"
# A MANIFEST MAY SHIP A ROW FOR A WATCHER THE OPERATOR HAS NOT CONFIGURED — a leading `?`
# marks it optional and it renders as kind `off`, so `status` grows a THIRD section below
# DEGRADED naming what is not installed. Two things in the hook read that output, and both
# were written when there were two sections:
#
#   - the DEGRADED block was taken as "everything after the word DEGRADED", so it swallowed
#     the NOT INSTALLED heading and its rows and reported an unconfigured watcher as running
#     and blind — an alert for a thing that is not a fault, in the section whose whole value
#     is that everything in it is one (law-alerts-must-be-actionable);
#   - the latch block named every row of the table, and `watchd.sh tail` refuses an `off`
#     row, so the one block meant to be pasted and run carried a command that cannot work.
#
# Both are live on a default installation, not hypothetical: the shipped manifest carries one
# such row and its key is empty until an operator sets it.
ORUN="$TMP/elsewhere/orun"; mkdir -p "$ORUN/watchd"
printf 'one event\n' > "$ORUN/watchd/answers.log"
printf 'one event\n' > "$ORUN/watchd/view.log"

OMANIFEST="$TMP/elsewhere/watchers.optional"
cat > "$OMANIFEST" <<EOF
answers|log|$ORUN/watchd/answers.log|echo "no local ids in the state file" >&2; exit 1
?view|daemon|@SPIRA_VIEW@ watch|true
EOF

ohook() { hook SessionStart startup SPIRA_RUN="$ORUN" SPIRA_WATCHERS="$OMANIFEST" "$@"; }

# THE POSITIVE CONTROL FIRST. The same row with its key SET is an ordinary daemon row, so
# everything asserted absent below is absent because the row is off and not because the
# fixture never produced a `view` watcher at all (law-absence-needs-a-positive-control).
oout="$(ohook SPIRA_VIEW=/bin/true)"
has "a configured optional row is a watcher like any other" "$oout" "view"

offout="$(ohook)"
has  "an unconfigured one is still named in the table"  "$offout" "view"
hasnt "no Monitor latch is emitted for it"              "$offout" "tail view"

has  "the degraded watcher still gets its section"      "$offout" "DEGRADED — running and blind"
has  "and its reason"                                   "$offout" "no local ids in the state file"
# THE BLEED, ASSERTED FROM BOTH ENDS: the heading must not appear, and neither must the text
# of the row underneath it — a fix that dropped only the word would still print the row.
hasnt "the section stops before what is merely not installed" "$offout" "NOT INSTALLED"
hasnt "so an unconfigured watcher is never called blind"      "$offout" "is not set in"

echo
echo "unconfigured-only manifest: table prints, no Monitor block"
# A manifest of nothing BUT unconfigured rows still has a table to print — the row is how an
# operator learns the watcher exists.
NMANIFEST="$TMP/elsewhere/watchers.none-on"
printf '?view|daemon|@SPIRA_VIEW@ watch|true\n' > "$NMANIFEST"
nout="$(hook SessionStart startup SPIRA_RUN="$ORUN" SPIRA_WATCHERS="$NMANIFEST")"
has  "the table still says the watcher exists" "$nout" "view"
hasnt "no Monitor command is printed"          "$nout" "Monitor:"

echo
echo "mail count line in the session hook"
# A CHECK THAT FINDS NOTHING MUST FIRST PROVE IT COULD HAVE FOUND SOMETHING. Plant one
# message and verify the line appears before believing silence on an empty mailbox.
printf 'From: Gate <gate@spira>\nSubject: A gate passed\nDate: Mon, 01 Jan 2024 00:00:00 +0000\n\nBody text here.\n' \
    > "$MAIL_DIR/concierge/new/1.msg"

mout="$(hook SessionStart startup)"
has "the mail count line is printed"   "$mout" "You have 1 unread"
has "and carries the list command"     "$mout" "mail.sh list concierge --unread"
hasnt "no message body is printed"    "$mout" "Body text here"
hasnt "no subject is printed"         "$mout" "A gate passed"
# NOTHING MOVES TO cur/. The hook peeks, it does not read.
is "nothing moved to cur/" "" "$(ls "$MAIL_DIR/concierge/cur/" | head -1)"

# COUNT CARRIES THE REAL NUMBER. Plant a second message and verify.
printf 'From: Gate <gate@spira>\nSubject: Another\nDate: Mon, 01 Jan 2024 00:00:01 +0000\n\nSecond body.\n' \
    > "$MAIL_DIR/concierge/new/2.msg"
mout2="$(hook SessionStart startup)"
has "count grows with more messages" "$mout2" "You have 2 unread"

# ZERO MEANS SILENCE. Remove the messages; the line must not appear.
rm "$MAIL_DIR/concierge/new/1.msg" "$MAIL_DIR/concierge/new/2.msg"
mout3="$(hook SessionStart startup)"
hasnt "zero unread: no mail line"    "$mout3" "You have 0 unread"
hasnt "and no list command either"   "$mout3" "mail.sh list concierge --unread"

# NO MONITOR INSTRUCTION FOR MAIL. The count line is informational; the reader opens their
# mail client themselves.
printf 'From: Gate <gate@spira>\nSubject: X\nDate: Mon, 01 Jan 2024 00:00:00 +0000\n\nX.\n' \
    > "$MAIL_DIR/concierge/new/3.msg"
mnout="$(hook SessionStart startup)"
is "no Monitor instruction is printed" "0" \
    "$(printf '%s\n' "$mnout" | grep -c 'Monitor: ' || true)"
rm "$MAIL_DIR/concierge/new/3.msg"

echo
echo "the registration in the client's settings file"
SET="$TMP/elsewhere/settings.json"
cat > "$SET" <<'EOF'
{
  "statusLine": { "command": "/some/meter.sh", "refreshInterval": 5 },
  "hooks": {
    "SessionStart": [
      { "matcher": "clear|startup|resume",
        "hooks": [ { "type": "command", "command": "/gone/hooks/old-session.sh" } ] }
    ]
  }
}
EOF
ish() { env -i HOME="$TMP/home" PATH="$TMP/bin:$PATH" SPIRA_CONF="$CONF" \
        bash "$CLONE/spira/install-session-hook.sh" "$@"; }

out="$(ish status)"; rc=$?
is  "status refuses to report an unregistered hook as fine" "1" "$rc"
has "and names the event it is missing from"   "$out" "MISSING SessionStart"
# A FOREIGN HOOK IS REPORTED, NEVER REMOVED. Two session hooks both reporting on watchers is
# the state this replaced, and which of them the operator wants is theirs to say.
has "a command that is not ours is reported"   "$out" "other"

ish install >/dev/null
out="$(ish status)"; rc=$?
is  "after install, status is clean"           "0" "$rc"
has "SessionStart carries the hook"            "$out" "ok      SessionStart"
has "and so does PostCompact"                  "$out" "ok      PostCompact"

# NO MATCHER AT ALL. An absent matcher matches every source; a matcher naming a subset is how
# a hook comes to be missing from `compact` and `fork`, both of which open a context window
# with no Monitor attached.
ours="$(python3 - "$SET" "$CLONE/spira/hooks/session.sh" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
for ev, entries in doc["hooks"].items():
    for e in entries:
        for h in e.get("hooks", []):
            if h.get("command") == sys.argv[2]:
                print("%s matcher=%r" % (ev, e.get("matcher")))
PY
)"
is "the SessionStart entry carries no matcher" "SessionStart matcher=None" \
   "$(printf '%s\n' "$ours" | grep '^SessionStart')"
is "and neither does PostCompact"              "PostCompact matcher=None" \
   "$(printf '%s\n' "$ours" | grep '^PostCompact')"

# INSTALLING TWICE LEAVES ONE ENTRY. It is run from `doctor` and by hand, so a second run that
# appended would grow the file without bound and run the hook twice per session start.
ish install >/dev/null
n="$(python3 - "$SET" "$CLONE/spira/hooks/session.sh" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
print(sum(1 for entries in doc["hooks"].values() for e in entries
          for h in e.get("hooks", []) if h.get("command") == sys.argv[2]))
PY
)"
is "installing twice leaves one entry per event" "2" "$n"

# AND EVERYTHING ELSE IN THE FILE SURVIVES. This is the operator's live client configuration
# and it holds settings nothing here knows about, which is why it is parsed and re-serialised
# rather than templated.
keep="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["statusLine"]["command"])' "$SET")"
is "unrelated settings are untouched" "/some/meter.sh" "$keep"
is "a backup of the prior file is left" "1" "$(ls "$SET.spira.bak" >/dev/null 2>&1 && echo 1 || echo 0)"

# PRUNE IS HOW A FOREIGN ENTRY IS REMOVED — by name, deliberately, one substring at a time.
out="$(ish prune /gone)"
has "prune names what it removed" "$out" "/gone/hooks/old-session.sh"
out="$(ish status)"
hasnt "and it is gone from status" "$out" "/gone/hooks/old-session.sh"
has  "while ours remains"          "$out" "ok      SessionStart"

ish uninstall >/dev/null
out="$(ish status)"; rc=$?
is "uninstall leaves nothing registered" "1" "$rc"
# AN ENTRY EMPTIED OF HOOKS IS REMOVED, not left as a husk: the client validates a matcher
# entry by its hooks being non-empty, so a husk turns a clean uninstall into a settings file
# reported as malformed.
husk="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(json.dumps(d.get("hooks","gone")))' "$SET")"
is "and no empty husk is left behind" '"gone"' "$husk"
keep="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["statusLine"]["command"])' "$SET")"
is "with the rest of the file still intact" "/some/meter.sh" "$keep"

echo
echo "install is a repair, so it must be safe to run on a timer"
# WRITES ONLY ON CHANGE. It runs from `cockpit-ensure` every minute as well as by hand; a
# version that rewrote on every pass would churn the operator's live client settings and its
# backup once a minute, and would report a change on every one of them.
ish install >/dev/null
before="$(cat "$SET")"
rm -f "$SET.spira.bak"
out="$(ish install)"; rc=$?
is "a second install exits clean"            "0" "$rc"
is "and changes nothing in the file"         "$before" "$(cat "$SET")"
is "and writes no backup, having written nothing" "0" \
   "$(ls "$SET.spira.bak" >/dev/null 2>&1 && echo 1 || echo 0)"
is "and says nothing"                        "" "$out"

# AND IT REFUSES A HOOK THAT IS NOT THERE. A registered command that does not exist is a hook
# error reported to the user at every session start — and that is exactly the state a timed
# repair falls into while the file it names is still arriving with a branch.
GONE="$TMP/gone-clone"
mkdir -p "$GONE/spira/hooks"
cp "$HERE/conf.sh" "$HERE/install-session-hook.sh" "$GONE/spira/"
SET2="$TMP/elsewhere/settings2.json"
out="$(env -i HOME="$TMP/home" PATH="$TMP/bin:$PATH" SPIRA_CONF="$CONF" \
       SPIRA_CLIENT_SETTINGS="$SET2" bash "$GONE/spira/install-session-hook.sh" install 2>&1)"; rc=$?
is  "install refuses when the hook is not there" "1" "$rc"
has "and names what is missing"                  "$out" "not executable"
is  "and registers nothing at all"               "0" \
    "$(ls "$SET2" >/dev/null 2>&1 && echo 1 || echo 0)"

echo
echo "the repair is wired, not merely available"
# A REGISTRATION DONE ONCE BY HAND ROTS INVISIBLY, and the rot is silent: a hook bound to a
# path whose harness was decommissioned goes on printing that harness's banner into every
# session on the box and looks, from inside one, exactly like a working hook. So the wiring is
# asserted rather than described — install writes it once, and a timer repairs it.
ROOT="$(cd "$HERE/.." && pwd -P)"
has "the installer registers it on a fresh box" \
    "$(cat "$ROOT/systemd/install.sh")" "install-session-hook.sh"
has "and a timed unit repairs it afterwards" \
    "$(cat "$ROOT/systemd/cockpit-ensure.service")" "install-session-hook.sh install"
# law-fence-loops-on-shared-hardware: the unit this was added to fires every minute.
has "that unit is fenced with a CPU quota" \
    "$(cat "$ROOT/systemd/cockpit-ensure.service")" "CPUQuota="

echo
echo "an aeon session is told nothing"
# AN AEON THAT SEES THE HOOK OUTPUT COULD OBEY THE WATCHER TABLE. SPIRA_AEON is exported by
# aeon.sh into every session it summons and is the definitive marker.
aout="$(hook SessionStart startup SPIRA_AEON=mindy)"
is  "an aeon session gets no output at all"     "" "$aout"
# THE POSITIVE CONTROL: the same fixture without SPIRA_AEON still produces the status table,
# so the silence above is about the guard and not about the fixture
# (law-absence-needs-a-positive-control).
pout="$(hook SessionStart startup)"
has "an operator session still gets the status table" "$pout" "## Spira watchers"

echo
tl_summary
