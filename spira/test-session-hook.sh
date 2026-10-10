#!/usr/bin/env bash
#
# test-session-hook.sh — the session hook's own output.
#
#   ./test-session-hook.sh
#
# WHAT IT HOLDS. The hook's output is prepended to a context window that has just opened,
# which fails silently by nature, so every property below is one that would otherwise be
# believed rather than known:
#
#   1. ONE LINE PER WATCHER, AND NOTHING ELSE. Output is one header line, one line per manifest
#      row, and at most one mail line — never a backlog preview. The assertion is on the exact
#      line count against a fixture manifest, because the failure being fixed was a hook that
#      printed 38 lines of watcher backlog plus a 409-line summary into a fresh context.
#   2. NO EVENT CONTENT LEAKS. `peek` is never called; a watcher's unread count is reported but
#      none of its log lines are, because the session re-attaches with `watchd tail <name>`,
#      which replays the same backlog from its cursor.
#   3. A DEGRADED ROW CARRIES ITS OWN REASON, inline on the same line — a watcher that is
#      running and blind is silent in exactly the way a healthy quiet one is, so the suite
#      plants one before believing that it can be absent (law-absence-needs-a-positive-control).
#   4. IT MARKS NOTHING READ. `peek`, not `drain` — the unread count is unchanged across two
#      hook calls.
#   5. IT NEVER BREAKS A SESSION START. No stdin, malformed stdin, no manifest, an unreadable
#      one: exit 0 every time, and silence where there is nothing to say.
#
# ITS REGISTRATION IN THE CLIENT'S OWN SETTINGS FILE moved to `release session-hook`
# (sp-7jr34) and with it every property about the registration itself — converging any number
# of earlier entries to one, the no-matcher/`PostCompact`-retiring shape, a foreign entry
# reported but never removed, `prune`, the backup-then-atomic-write — which are now
# `release/src/session_hook.rs`'s own unit tests (`cargo test -p release`), plus one
# regression test run under the client's real minimal environment
# (`release/tests/session_hook_minimal_env.rs`). What is left of that concern here is "the
# repair is wired, not merely available" below: that this tree's own call sites still invoke
# it, which no Rust test can see.
#
# It needs no database and no beads server. Every configured value is pinned to a NON-DEFAULT,
# so a literal written into the hook cannot pass by coincidence, and nothing here can reach
# the operator's own configuration or their live client settings file.
#
# defect: sp-4vp
# tier: T1
# covers: release/src/session_hook.rs spira/hooks/session.sh watchd/* inbox-triage/* mail/src/* install/src/bin/units_install.rs systemd/cockpit-ensure.service
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "$2" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1" "$2" ;; *) ok "$1" ;; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home" "$TMP/bin"

# conf.sh's spira.toml auto-convert shells out to spira-config (sp-zs04v.2), found on PATH.
command -v spira-config >/dev/null 2>&1 || bail "spira-config is not on PATH"
# sp-48f6g: watchd.sh rewritten to the compiled binary `watchd`; the session hook drives it
# as a subprocess (law-prefer-the-real-dependency, not a stub — see PART 6 below).
command -v watchd >/dev/null 2>&1 || bail "watchd is not on PATH"
# sp-ooh1k: mail.sh rewritten to the compiled binary `mail`, same treatment.
command -v mail >/dev/null 2>&1 || bail "mail is not on PATH"

# A harness tree that is NOT this checkout, so nothing here can read the operator's own
# configuration, their watcher manifest or their client settings and report a pass it did not
# earn.
CLONE="$TMP/clone"
mkdir -p "$CLONE/spira/hooks"
cp "$HERE/conf.sh" "$CLONE/spira/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$CLONE/spira/"
cp "$HERE/hooks/session.sh" "$CLONE/spira/hooks/"
# `watchd`, `inbox-triage` (sp-48f6g) and `mail` (sp-ooh1k) are compiled binaries, not
# scripts under spira/ to copy — the hook finds them on PATH, same as spira-config above.

# `status` asks systemd about every daemon row. A stub answers instead, so this suite says
# nothing about whether the box it runs on has a user manager.
cat > "$TMP/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
for a in "$@"; do case "$a" in spira-watch@*) echo active ;; esac; done
exit 0
EOF
chmod +x "$TMP/bin/systemctl"

# EVERY CONFIGURED VALUE PINNED TO A NON-DEFAULT. SPIRA_RUN would derive to $CLONE/.runtime and
# SPIRA_WATCHERS to $CLONE/spira/watchers; both are moved, so a literal written into the hook
# cannot pass.
RUN="$TMP/elsewhere/run"; mkdir -p "$RUN/watchd"
MANIFEST="$TMP/elsewhere/watchers"
MAIL_DIR="$TMP/elsewhere/mail"
mkdir -p "$MAIL_DIR/concierge/new" "$MAIL_DIR/concierge/cur" "$MAIL_DIR/concierge/tmp"
# THE FIXTURE DECLARES ITSELF IN FORCE. The hook refuses to print from a harness that is not
# the one systemd runs, so a clone that left SPIRA_PROD at its derived default would be silent
# here and every assertion below would pass on an empty string.
#
# conf.sh no longer reads a legacy spira.conf at all (per Ryan 2026-10-05, the
# one-source-of-config law — "no legacy spira.conf, no conversion"), so these are declared
# through tl_config/SPIRA_TOML instead of a hand-written $TMP/spira.conf.
# SPIRA_CONCIERGE_INBOX undeclared resolves to the complete fixture's own
# /fixture/userhome/spira/run/watchd/concierge-inbox.log — the hook (and mail, underneath it)
# reads/appends it directly, and that path does not exist here (sfail round 3, pattern 7).
tl_config SPIRA_ID_PREFIX=sp SPIRA_PROD="$CLONE/spira" SPIRA_RUN="$RUN" \
    SPIRA_WATCHERS="$MANIFEST" SPIRA_CLIENT_SETTINGS="$TMP/elsewhere/settings.json" \
    SPIRA_MAIL="$MAIL_DIR" SPIRA_MAIL_INDEX="$MAIL_DIR/index" SPIRA_MAIL_SESSION_MAILBOX=concierge \
    SPIRA_CONCIERGE_INBOX="$RUN/watchd/concierge-inbox.log"

cat > "$MANIFEST" <<'EOF'
answers|daemon|/bin/sleep 3600
cron|log|@SPIRA_RUN@/watchd/somebody-elses.log
EOF

# THE BACKLOG. A distinguishing marker in the content, never in a summary line — that is what
# proves the difference between a count and a preview.
python3 - "$RUN/watchd/answers.log" <<'PY'
import sys
with open(sys.argv[1], "w") as fh:
    for i in range(300):
        fh.write("ESCALATED-MARKER sp-%03d wants a decision\n" % i)
PY
: > "$RUN/watchd/somebody-elses.log"

# hook <event> <source> [env...] — run the hook exactly as the client does: the payload on
# stdin, in a minimal environment that cannot reach the operator's configuration.
#
# A registered key passed in [env...] (SPIRA_WATCHERS, SPIRA_RUN, SPIRA_VIEW) is declared
# through tl_config instead — session.sh's conf.sh resolves these from SPIRA_TOML only, never
# from this process's environment — and dropped from what is forwarded to env -i; everything
# else (SPIRA_AEON, SPIRA_CONCIERGE, a PATH override) is forwarded exactly as before.
hook() {
    local ev="$1" src="$2"; shift 2
    local extra=() _a
    for _a in "$@"; do
        case "$_a" in
            SPIRA_WATCHERS=*|SPIRA_RUN=*|SPIRA_VIEW=*) tl_config "$_a" ;;
            *) extra+=("$_a") ;;
        esac
    done
    # SPIRA_RELEASE (not SPIRA_HOME) is what the real registered hook command sets
    # (release/src/session_hook.rs's hook_command: "env SPIRA_RELEASE=<releases>/current ...
    # <releases>/current/spira/hooks/session.sh") — session.sh can source conf.sh via its own
    # BASH_SOURCE, but the compiled `watchd` binary it shells out to has no such trick and
    # needs SPIRA_HOME/SPIRA_RELEASE in its own environment (locate_home: "named, never
    # searched for"). Without it every watchd call here refused "neither SPIRA_HOME nor
    # SPIRA_RELEASE is set", which the hook swallows into a generic "status unavailable" line.
    printf '{"hook_event_name":"%s","source":"%s"}' "$ev" "$src" \
      | env -i HOME="$TMP/home" PATH="$TMP/bin:$CLONE/spira:$PATH" \
        SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF=/nonexistent SPIRA_CONFIG_WRITE=1 \
        SPIRA_RELEASE="$CLONE" \
        "${extra[@]+"${extra[@]}"}" \
        bash "$CLONE/spira/hooks/session.sh"
}

echo "one line per watcher, and nothing else — the positive control"
# A CHECK THAT FINDS NOTHING MUST FIRST PROVE IT COULD HAVE FOUND SOMETHING. Every absence
# asserted below is believable only because this passes: the hook can read a real manifest and
# a real backlog, so silence later is about the case and not about the fixture.
out="$(hook SessionStart startup)"; rc=$?
is  "the hook exits clean"                       "0" "$rc"
has "it names itself and how to re-attach"       "$out" "## Spira watchers — re-attach with:"
has "the re-attach command names watchd tail"    "$out" "watchd tail <name>"
has "the answers row is there, with its count"   "$out" "answers"
has "and its unread count"                       "$out" "300 unread"
has "and so is the log row"                      "$out" "cron"
n="$(printf '%s\n' "$out" | wc -l)"
# THE ACCEPTANCE CRITERION: one header line plus one line per manifest row (two rows here) —
# never a preview, whatever the backlog's size.
is "output is exactly 1 header + 1 line per row" "3" "$n"

echo
echo "no event content leaks — peek is never called from the hook"
# A 300-EVENT BACKLOG WOULD HAVE FILLED THE OLD PREVIEW WITH THIS MARKER. Its absence here,
# together with the accurate count above, is what shows the hook reports on the backlog rather
# than replaying any of it.
hasnt "no raw event text in the output"          "$out" "ESCALATED-MARKER"

echo
echo "it marks nothing read"
# IF THE HOOK DRAINED, the count would fall after the first read. The cursor is the whole
# evidence: the contract is a log and an integer, and the integer must not have moved.
is  "no cursor file is written"                  "" "$(ls "$RUN/watchd" | grep cursor || true)"
out2="$(hook SessionStart clear)"
has "a second session sees the same count"       "$out2" "300 unread"

echo
echo "no Monitor instructions are printed"
# Delivery and replies now handle operator communication through mail; the Monitor latch is
# retired.
hasnt "no Monitor instruction is emitted"         "$out" "Monitor:"
hasnt "no resume-from-cursor text"                "$out" "Nothing above was marked read"
# IT NEVER SENDS THE READER TO ListAgents. ListAgents enumerates agents and sessions and no
# tool enumerates a session's own Monitors, so "attach only the streams not already listed
# there" reported nothing attached every time (sp-vv4p, superseded).
hasnt "it does not send the reader to ListAgents" "$out" "ListAgents"

echo
echo "every session-start source is summarised, none is special"
# A SessionStart carries one of five sources and every one of them opens a context window with
# no Monitor attached. `compact` and `fork` are the two a hand-written matcher omits, so they
# are the two asserted here.
for src in startup resume clear compact fork; do
    got="$(hook SessionStart "$src")"
    has "source '$src' is summarised" "$got" "## Spira watchers"
done

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
    printf '%s' "$1" | env -i HOME="$TMP/home" PATH="$TMP/bin:$CLONE/spira:$PATH" \
        SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF=/nonexistent SPIRA_CONFIG_WRITE=1 \
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
echo "a watchd that fails transiently is retried, then reported — never silently dropped"
FLAKE="$TMP/flake"; mkdir -p "$FLAKE"
cat > "$FLAKE/watchd" <<SH
#!/usr/bin/env bash
n=\$(cat "$FLAKE/n" 2>/dev/null || echo 0); echo \$((n+1)) > "$FLAKE/n"
[ "\$n" -lt "\${FLAKE_FAILS:-0}" ] && { echo "watchd: timed out" >&2; exit 1; }
printf 'NAME UNIT HEALTH UNREAD LAST-EVENT RESTARTS LOG\nanswers active OK 0 - 0 /x\n'
SH
chmod +x "$FLAKE/watchd"
fhook() { hook SessionStart resume PATH="$FLAKE:$TMP/bin:$CLONE/spira:$PATH" "$@"; }
rm -f "$FLAKE/n"; fo="$(fhook FLAKE_FAILS=1)"
has "one failed read is retried and the table appears" "$fo" "answers OK (active)"
rm -f "$FLAKE/n"; fo="$(fhook FLAKE_FAILS=99)"
has "a persistent failure says status is unavailable" "$fo" "status unavailable"

echo
echo "a DEGRADED row carries its own reason, inline"
# DRIVEN THROUGH THE REAL `watchd`, never a planted table (law-prefer-the-real-dependency).
# A health probe that exits non-zero is all the real thing needs, so there is nothing here
# worth faking.
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
has "the degraded watcher's row is there"     "$dout" "answers DEGRADED"
# THE REASON, NOT JUST THE WORD, AND ON THE SAME LINE. `DEGRADED` alone says a check failed and
# not which, so the next step would be to re-run the probe by hand — the work this output
# exists to have done.
has "and it carries the probe's own reason, on the row" \
    "$(printf '%s\n' "$dout" | grep '^answers DEGRADED')" "no local ids in the state file"
has "the healthy row has no such text"        "$dout" "cron OK"

hasnt "no Monitor instruction in DEGRADED case"   "$dout" "Monitor:"

hout="$(dhook "$DOK")"
hasnt "a healthy manifest raises no DEGRADED row" "$hout" "DEGRADED"
has  "but the table is still printed"             "$hout" "answers"
hasnt "no Monitor instruction in healthy case"    "$hout" "Monitor:"

echo
echo "a watcher this installation has not got is shown, and never latched"
# A MANIFEST MAY SHIP A ROW FOR A WATCHER THE OPERATOR HAS NOT CONFIGURED — a leading `?`
# marks it optional and it renders as kind `off`, with every column `-`.
ORUN="$TMP/elsewhere/orun"; mkdir -p "$ORUN/watchd"
printf 'one event\n' > "$ORUN/watchd/answers.log"
printf 'one event\n' > "$ORUN/watchd/view.log"

OMANIFEST="$TMP/elsewhere/watchers.optional"
cat > "$OMANIFEST" <<EOF
answers|log|$ORUN/watchd/answers.log|true
?view|daemon|@SPIRA_VIEW@ watch|true
EOF

ohook() { hook SessionStart startup SPIRA_RUN="$ORUN" SPIRA_WATCHERS="$OMANIFEST" "$@"; }

# THE POSITIVE CONTROL FIRST. The same row with its key SET is an ordinary daemon row, so
# everything asserted absent below is absent because the row is off and not because the
# fixture never produced a `view` watcher at all (law-absence-needs-a-positive-control).
oout="$(ohook SPIRA_VIEW=/bin/true)"
has "a configured optional row is a watcher like any other" "$oout" "view"

# Reset SPIRA_VIEW back to "not configured" — tl_config persists, unlike the old per-call
# env prefix, so the positive control's /bin/true above would otherwise leak into this
# negative control. "Not configured" means truly EMPTY (watchd/src/manifest.rs's optional-row
# rule: an empty key makes the row Kind::Off, never checked for DEGRADED; a non-empty value,
# even a nonexistent path, is "configured but broken" and DOES show DEGRADED). The complete
# fixture's own declared default is a non-empty nonexistent path — exactly that "configured
# but broken" case, not "unconfigured" — so it must be overridden to "" here, not to a path.
tl_config SPIRA_VIEW=""
offout="$(ohook)"
has  "an unconfigured one is still named in the table"  "$offout" "view"
hasnt "no Monitor latch is emitted for it"              "$offout" "tail view"
hasnt "and it is never reported as DEGRADED"            "$offout" "view DEGRADED"

echo
echo "mail count line in the session hook"
# A CHECK THAT FINDS NOTHING MUST FIRST PROVE IT COULD HAVE FOUND SOMETHING. Plant one
# message and verify the line appears before believing silence on an empty mailbox.
printf 'From: Gate <gate@spira>\nSubject: A gate passed\nDate: Mon, 01 Jan 2024 00:00:00 +0000\n\nBody text here.\n' \
    > "$MAIL_DIR/concierge/new/1.msg"

mout="$(hook SessionStart startup)"
has "the mail count line is printed"   "$mout" "You have 1 unread"
has "and carries the list command"     "$mout" "mail list concierge --unread"
hasnt "no message body is printed"    "$mout" "Body text here"
hasnt "no subject is printed"         "$mout" "A gate passed"
# NOTHING MOVES TO cur/. The hook peeks, it does not read.
is "nothing moved to cur/" "" "$(ls "$MAIL_DIR/concierge/cur/" | head -1)"
n="$(printf '%s\n' "$mout" | wc -l)"
is "the mail line is the only addition to the count" "4" "$n"

# COUNT CARRIES THE REAL NUMBER. Plant a second message and verify.
printf 'From: Gate <gate@spira>\nSubject: Another\nDate: Mon, 01 Jan 2024 00:00:01 +0000\n\nSecond body.\n' \
    > "$MAIL_DIR/concierge/new/2.msg"
mout2="$(hook SessionStart startup)"
has "count grows with more messages" "$mout2" "You have 2 unread"

# ZERO MEANS SILENCE. Remove the messages; the line must not appear.
rm "$MAIL_DIR/concierge/new/1.msg" "$MAIL_DIR/concierge/new/2.msg"
mout3="$(hook SessionStart startup)"
hasnt "zero unread: no mail line"    "$mout3" "You have 0 unread"
hasnt "and no list command either"   "$mout3" "mail list concierge --unread"

# NO MONITOR INSTRUCTION FOR MAIL. The count line is informational; the reader opens their
# mail client themselves.
printf 'From: Gate <gate@spira>\nSubject: X\nDate: Mon, 01 Jan 2024 00:00:00 +0000\n\nX.\n' \
    > "$MAIL_DIR/concierge/new/3.msg"
mnout="$(hook SessionStart startup)"
is "no Monitor instruction is printed" "0" \
    "$(printf '%s\n' "$mnout" | grep -c 'Monitor: ' || true)"
rm "$MAIL_DIR/concierge/new/3.msg"

echo
echo "the repair is wired, not merely available"
# A REGISTRATION DONE ONCE BY HAND ROTS INVISIBLY, and the rot is silent: a hook bound to a
# path whose harness was decommissioned goes on printing that harness's banner into every
# session on the box and looks, from inside one, exactly like a working hook. So the wiring is
# asserted rather than described — install writes it once, and a timer repairs it.
#
# THE REGISTRATION MECHANICS THEMSELVES (sp-7jr34) moved into the `release` crate as
# `release session-hook`, which owns: converging any number of earlier Spira entries — a
# sha-pinned path, the bare "current" path, wrapped or not — to exactly one on both
# `SessionStart` and `statusLine`; the no-matcher, `PostCompact`-retiring shape; reporting a
# foreign hook without ever removing it; `prune <substring>` as the deliberate removal path;
# the backup-then-atomic-write and "no change, no write" properties; and refusing when the
# hook/meter file is not executable. All of that is `release/src/session_hook.rs`'s own unit
# tests now (`cargo test -p release`), run on every change to that file rather than only when
# this suite happens to run. `release/tests/session_hook_minimal_env.rs` covers sp-7jr34's own
# regression directly: the registered command, run under the client's real minimal
# environment, succeeds — and the OLD unwrapped form does not. What is left here is the one
# thing those tests cannot see: that the CALL SITES in this tree actually invoke it.
ROOT="$(cd "$HERE/.." && pwd -P)"
# sp-31dm0: systemd/install.sh is retired; units-install (install/src/bin/units_install.rs)
# calls `release session-hook install` unconditionally now, in the same place (sp-7jr34).
has "the installer registers it on a fresh box" \
    "$(cat "$ROOT/install/src/bin/units_install.rs")" "release session-hook install"
has "and a timed unit repairs it afterwards" \
    "$(cat "$ROOT/systemd/cockpit-ensure.service")" "release session-hook install"

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
echo "the concierge's mandatory first action — only on the concierge socket, every source"
# THE POSITIVE CONTROL FIRST: without SPIRA_CONCIERGE, no arm instruction at all, so the
# presence asserted below is about the guard and not about the fixture
# (law-absence-needs-a-positive-control).
nout="$(hook SessionStart startup)"
hasnt "a non-concierge session gets no arm instruction" "$nout" "MANDATORY FIRST ACTION"

cout="$(hook SessionStart startup SPIRA_CONCIERGE=1)"
has "the concierge session gets the arm instruction"    "$cout" "MANDATORY FIRST ACTION"
has "naming watchd next as the background job to run"    "$cout" "next concierge-inbox"
hasnt "and not a Monitor to arm"                         "$cout" "Monitor command="
has "naming the durable inbox path"                      "$cout" "$RUN/watchd/concierge-inbox.log"
has "it still gets the ordinary watcher table too"       "$cout" "## Spira watchers"

# EVERY SessionStart SOURCE, NOT JUST startup — a compaction is the case this exists for.
for src in startup resume clear compact fork; do
    got="$(hook SessionStart "$src" SPIRA_CONCIERGE=1)"
    has "source '$src' also gets the arm instruction" "$got" "MANDATORY FIRST ACTION"
done

# SessionEnd HAS NOTHING TO SAY EVEN FOR THE CONCIERGE — the context it would print into is
# the one going away.
endout="$(hook SessionEnd clear SPIRA_CONCIERGE=1)"
is "SessionEnd gives the concierge nothing either" "" "$endout"

# THE UNREAD COUNT IS REAL, NOT A PLACEHOLDER. Plant lines in the durable inbox and confirm
# the number in the arm instruction reflects them.
printf 'one\ntwo\nthree\n' > "$RUN/watchd/concierge-inbox.log"
n_out="$(hook SessionStart startup SPIRA_CONCIERGE=1)"
has "the arm instruction carries the real unread count" "$n_out" "(3 lines)"
rm -f "$RUN/watchd/concierge-inbox.log"

# AN AEON IS NEVER A CONCIERGE, but the guard ordering matters: SPIRA_AEON must win even if
# SPIRA_CONCIERGE were somehow also set.
aeon_out="$(hook SessionStart startup SPIRA_AEON=mindy SPIRA_CONCIERGE=1)"
is "an aeon session gets nothing, even with SPIRA_CONCIERGE set" "" "$aeon_out"

echo
tl_summary
