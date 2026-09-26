#!/usr/bin/env bash
#
# test-concierge.sh — the Concierge persona: brief composition, resume/HEADLESS handling,
#   and the ways the operator's way in has broken before. No systemd, no tmux, no live
#   statute book — see test-concierge-acceptance.sh for the host-only remainder (cgroup
#   survival, the launcher under a real systemd oneshot, the cockpit layout pane) and
#   test-concierge-roster.sh for the roster, resume-id and session-hook seams.
#
# UC-operator-channel-41 — DEMOTED TO T1 (coverage map row 41): the brief-composition
# section used to skip outright when the live statute book was empty, so a container run
# and a full run reported the same green for a different number of assertions. It now
# drives compose_brief through the SPIRA_MEMORIES_CMD seam against a fixture persona and a
# fixture chamber, always: the mechanism under test (no leftover {{, an executable mail.sh
# and bead.sh, the statute book present, a typo'd core set refusing rather than composing
# silently) does not depend on what is currently enacted in the real book.
#
# defect: sp-u4x
# covers: spira/lib.sh concierge.sh spira/chamber/concierge.fayth spira/chamber/concierge.md UC-operator-channel-40 UC-operator-channel-41
# hermetic-ok: fixture chamber and a private tmux socket; no systemd and no database
# requires: claude
# host-reason: concierge.sh refuses to run any subcommand without claude and tmux on PATH (spira_require); the `here` section also needs a real tmux binary and skips on its own if one is not on PATH
# scar: the concierge persona lacked FAYTH_SUMMON=operator and could be claimed by the sentinel as an ordinary worker.
# tier: T2
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
HARNESS="$(cd "$HERE/.." && pwd)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "the escalation list — three classes, not the old five (sp-3dggv)"

# SEEN TO FAIL FIRST: before sp-3dggv, concierge.md escalated on five-plus classes, including
# "anything that will page him" and "a product decision about what a feature IS or what a
# number MEANS" — prose the operator's verdict retired. Pin the persona text to the amended
# law-escalate-decisions-not-problems statute so the two cannot drift apart silently.
CM="$(cat "$HERE/chamber/concierge.md")"
want   "names PERMISSIONS"                                          "PERMISSIONS" "$CM"
want   "names POLICY"                                                "POLICY"      "$CM"
want   "names DESTRUCTIVE"                                           "DESTRUCTIVE" "$CM"
nowant "no longer escalates on 'anything that will page'"            "anything that will page" "$CM"
nowant "no longer escalates on the retired product-decision class"   "product decision about what a feature IS" "$CM"
nowant "no longer escalates on work outside an approved design's intent" "approved design" "$CM"
nowant "no longer escalates on a choice between defensible options"  "choice between defensible options" "$CM"

echo
echo "the brief — argument handling"

# BRIEF REJECTS EXTRA ARGUMENTS: 'brief --resume' exits 2 and names the working composition.
# An operator who types it expects an error, not silent success with the flag dropped.
out="$(bash "$HARNESS/concierge.sh" brief --resume 2>&1)"; rc=$?
is   "brief --resume exits 2"              2 "$rc"
want "and names the working composition"   "append-system-prompt" "$out"
# POSITIVE CONTROL: without extra arguments the exit code is not 2, confirming the check
# fires on the flag and not on something unrelated.
rc_plain=0; bash "$HARNESS/concierge.sh" brief 2>/dev/null >/dev/null || rc_plain=$?
if [ "$rc_plain" -ne 2 ]; then
    ok "brief without args does not exit 2"
else
    bad "brief without args does not exit 2" "positive control broken"
fi

echo
echo "the brief — composed against a fixture chamber and a fixture statute cache"

# A FIXTURE PERSONA, NOT THE SHIPPED ONE — editing chamber/concierge.fayth to drive this
# would leave the suite one failed assertion away from having corrupted the thing it tests.
# mail.sh and bead.sh are SYMLINKED IN rather than reimplemented, so "names an executable
# tool" is checking the real tools under a fixture chamber, not a fixture's stand-ins.
FX="$TMP/fx"; mkdir -p "$FX/chamber"
cp "$HERE/chamber/concierge.md" "$FX/chamber/fx.md"
ln -sf "$HERE/mail.sh" "$FX/mail.sh"
ln -sf "$HERE/bead.sh" "$FX/bead.sh"
cat > "$FX/chamber/fx.fayth" <<'EOF'
FAYTH_NAME=fx
FAYTH_STATUTE_CORE="law-rm-alpha"
EOF

FIX_JSON='{"law-rm-alpha":"Alpha fixture statute body.","law-rm-beta":"Beta fixture statute body."}'
fixture_cmd() { printf 'printf %s' "$(printf '%q' "$FIX_JSON")"; }

brief_fx() { # brief_fx [persona] -> compose the brief for a fixture persona
    SPIRA_HOME="$FX" CONCIERGE_FAYTH="${1:-fx}" SPIRA_MEMORIES_CACHE="" \
        SPIRA_MEMORIES_CMD="$(fixture_cmd)" bash "$HARNESS/concierge.sh" brief
}

BRIEF="$(brief_fx 2>"$TMP/err")"
if [ -n "$BRIEF" ] && [ -f "$BRIEF" ]; then
    ok "concierge.sh brief renders a file"
    B="$(cat "$BRIEF")"
    # EVERY PLACEHOLDER, because an unsubstituted one is a command line the session will try
    # to run. The failure arrives hours later as "the concierge does not escalate anything".
    nowant "no placeholder survives rendering"  "{{"          "$B"
    want   "the brief names the mail path"      "mail.sh send operator"  "$B"
    mail_path="$(printf '%s\n' "$B" | grep -oE '[^ `]+mail\.sh' | head -1)"
    if [ -n "$mail_path" ] && [ -x "$mail_path" ]; then
        ok "mail path in brief exists and is executable: $mail_path"
    else
        bad "mail path in brief exists and is executable" "[${mail_path:-<not found>}]"
    fi
    want   "and the bead contract"              "bead.sh file" "$B"
    # THE FILE, NOT THE STRING. The string check above is the positive control: the path
    # must be named for the grep below to find it. The assertion with teeth is this one —
    # a path that is named but absent passes the string check and fails here.
    bead_path="$(printf '%s\n' "$B" | grep -oE '[^ ]+bead\.sh' | head -1)"
    if [ -n "$bead_path" ] && [ -x "$bead_path" ]; then
        ok "bead tool path in brief exists and is executable: $bead_path"
    else
        bad "bead tool path in brief exists and is executable" "[${bead_path:-<not found>}]"
    fi
    want "and carries the statute book"       "# Memories in force" "$B"
    want "the declared core statute renders in full" "## law-rm-alpha" "$B"
    want "and its body is present"                   "Alpha fixture statute body" "$B"
else
    bad "concierge.sh brief produced nothing" "$(cat "$TMP/err")"
fi

# A MISSING BRIEF IS A REFUSAL, NOT A DEGRADED START — the assertion that the refusal exists.
# Pointed at a persona with no markdown, it must fail loudly rather than launch a session whose
# only difference from a working one is that it was never told anything.
out="$(brief_fx no-such-persona 2>&1)"; rc=$?
is   "a missing brief exits non-zero"     1 "$rc"
want "and says which file was missing"    "no-such-persona.md" "$out"

# A CORE SET THAT RENDERS NOTHING IN FULL IS A TYPO, AND IT IS THE SILENT ONE. render_memories
# matches core slugs EXACTLY and demotes anything it does not recognise to the index tier
# without a word, so a mistyped or retired slug costs that statute its full text and says
# nothing at all. The brief still looks complete — right size, every placeholder filled, the
# law apparently present — which is why this needs an assertion rather than a reader.
cat > "$FX/chamber/typo.md" <<EOF
$(cat "$HERE/chamber/concierge.md")
EOF
cat > "$FX/chamber/typo.fayth" <<'EOF'
FAYTH_NAME=typo
FAYTH_STATUTE_CORE="law-slug-that-does-not-exist"
EOF
out="$(brief_fx typo 2>&1)"; rc=$?
is   "an all-typo core set exits non-zero"  1 "$rc"
want "and says the slugs were demoted"      "no statute rendered in full" "$out"

echo
echo "the brief — no-wiki install (SPIRA_WIKI unset)"

# SPIRA_WIKI UNSET: every path the brief names must be a file that exists on this host.
# THE POSITIVE CONTROL: the brief must still name the bead tool; absence of a wiki-relative
# path alone would pass just as well against a brief that named nothing at all.
BRIEF_NW="$(SPIRA_WIKI= brief_fx 2>"$TMP/err_nw")"
if [ -n "$BRIEF_NW" ] && [ -f "$BRIEF_NW" ]; then
    ok "no-wiki brief renders"
    BNW="$(cat "$BRIEF_NW")"
    nowant "no-wiki brief does not name a wiki-relative tool"  ".claude/bead.sh" "$BNW"
    want   "no-wiki brief still names the harness bead tool"   "bead.sh file"    "$BNW"
    bead_path="$(printf '%s\n' "$BNW" | grep -oE '[^ ]+bead\.sh' | head -1)"
    if [ -n "$bead_path" ] && [ -f "$bead_path" ]; then
        ok "bead tool path in brief exists: $bead_path"
    else
        bad "bead tool path in brief exists" "[${bead_path:-<not found>}]"
    fi
else
    bad "no-wiki brief renders" "$(cat "$TMP/err_nw")"
fi

echo
echo "resume — launcher carries SPIRA_CONCIERGE=1, and the retry mechanism is in the script"

want "the launcher source contains SPIRA_CONCIERGE export" \
    "SPIRA_CONCIERGE=1" "$(cat "$HARNESS/concierge.sh")"

echo
echo "here — convergence: attach when session exists; start-then-attach otherwise"

# SEEN TO FAIL FIRST: old `here` composed a brief and launched a standalone claude in every
# path. New `here` attaches when the session exists (no brief composed), and calls start
# when it does not. The discriminating fact: compose_brief outputs "N statutes in full" to
# stderr; a path that skips it produces no such line. Uses a real tmux socket (no systemd
# needed for either branch of `here`).
if ! command -v tmux >/dev/null 2>&1; then
    printf '1..0 # SKIP no tmux on PATH — here/attach convergence requires it\n'
    exit 77
fi
HERE_SOCK="test-here-conv-$$"
tmux -L "$HERE_SOCK" kill-server 2>/dev/null || true

# POSITIVE CONTROL: with no session, here calls start → compose_brief → fails (no brief for
# a fixture persona with no chamber here). The failure names the missing brief, confirming
# the code path reaches start.
here_nostart="$(SPIRA_HOME="$TMP/empty-chamber" CONCIERGE_FAYTH=concierge \
    SPIRA_RUN="$TMP" SPIRA_WIKI="$TMP/fakebrain" SPIRA_CONF="$TMP/no.conf" \
    CONCIERGE_SOCKET="$HERE_SOCK" CONCIERGE_SESSION="$HERE_SOCK" \
    bash "$HARNESS/concierge.sh" here 2>&1)" || true
want "here calls start when no session (brief-composition error visible)" "no brief at" "$here_nostart"

# THE PROPERTY: when the session exists, here exec-attaches — no brief is composed.
tmux -L "$HERE_SOCK" new-session -d -s "$HERE_SOCK" 2>/dev/null
here_out="$(SPIRA_RUN="$TMP" SPIRA_WIKI="$TMP/fakebrain" SPIRA_CONF="$TMP/no.conf" \
    CONCIERGE_SOCKET="$HERE_SOCK" CONCIERGE_SESSION="$HERE_SOCK" \
    bash "$HARNESS/concierge.sh" here 2>&1)" || true
nowant "here does not compose brief when session exists (no second client)" \
    "no brief at" "$here_out"

tmux -L "$HERE_SOCK" kill-server 2>/dev/null || true

echo
echo "the ensure unit leaves the tmux server it starts alive"
svc="$(sed -n '/^\[Service\]/,/^\[/p' "$HARNESS/systemd/concierge.service" | grep -v '^\s*#')"
want "the unit is a oneshot, whose cgroup is reaped when start returns" "Type=oneshot" "$svc"
# THE MECHANISM IS IN THE SCRIPT, not the unit: concierge.sh start wraps tmux new-session
# with systemd-run --remain-after-exit, putting the server in its own transient cgroup that
# outlives the oneshot. This assert confirms the mechanism is visible in the script where
# maintainers look; the behavioural version (does the session actually survive) is host-only
# and lives in test-concierge-acceptance.sh.
want "start escapes the oneshot cgroup via systemd-run --remain-after-exit" \
    "remain-after-exit" "$(cat "$HARNESS/concierge.sh")"

echo
echo "duplicate client detection"

# (b) a second start/here while one holds the id does not launch a second client.
# concierge_live_pid finds a process by scanning /proc/*/cmdline for the session id.
#
# SEEN TO FAIL FIRST: with a non-matching id the pid must NOT be returned, proving the scan
# is driven by the id and not by "is any claude running".
LP_SID="live-pid-test-$(date +%s)"
lp() { SPIRA_RUN="$TMP" SPIRA_WIKI="$TMP/fakebrain" SPIRA_CONF="$TMP/no.conf" \
        bash "$HARNESS/concierge.sh" _live-pid "$1" 2>/dev/null; }

# POSITIVE CONTROL: a non-existent id must return nothing.
is "no pid for an id no process holds" "" "$(lp "definitely-not-in-any-cmdline-$$")"

# THE PROPERTY UNDER TEST: launch a background sleep with the id in its argv and find its pid.
bash -c "exec -a claude python3 -c 'import time; time.sleep(10)' --resume ${LP_SID}" &
LP_PID=$!
sleep 0.3
trap 'kill "$LP_PID" 2>/dev/null; rm -rf "$TMP"' EXIT

found="$(lp "$LP_SID")" || found=""
is "live pid is found when a process holds the id" "$LP_PID" "$found"

# IDENTITY, NOT A MENTION (2026-09-25): a process that only has the id somewhere in its
# command line — a tail of the transcript, the caller's own shell — is not a holder. The old
# substring scan reported one as a HEADLESS concierge and refused every start.
LP_MENTION="mention-only-$(date +%s)-$$"
bash -c "exec python3 -c 'import time; time.sleep(10)' $LP_MENTION" &
LP_MPID=$!; sleep 0.3
is "a process that only mentions the id is not a holder" "" "$(lp "$LP_MENTION")"
kill "$LP_MPID" 2>/dev/null; wait "$LP_MPID" 2>/dev/null || true

# PROCESS GONE: pid is no longer returned after the process exits.
kill "$LP_PID" 2>/dev/null; wait "$LP_PID" 2>/dev/null || true
is "pid is gone after the process exits" "" "$(lp "$LP_SID")"
trap 'rm -rf "$TMP"' EXIT  # restore trap without the kill

echo
echo "convergence — a live holder with no tmux session is HEADLESS, not convergence (D10: was two tests)"

# SEEN TO FAIL FIRST: the old behaviour was exit 0 (treating the headless case as
# convergence). The correct behaviour is exit 3 (HEADLESS): has-session already failed, so a
# live holder with no session means the tmux server is gone, and no second client is spawned.
# `claude` is stubbed on PATH so "no client was launched" is checked directly rather than
# inferred, and the process is a real `python3 -c 'time.sleep'` holding the id in its argv —
# not tmux, so this needs neither systemd nor tmux to run.
CONV_SID="conv-test-$(date +%s)"
CONV_DIR="$(mktemp -d)"; mkdir -p "$CONV_DIR/bin"
printf '#!/bin/sh\necho STUB_CLAUDE_RAN\n' > "$CONV_DIR/bin/claude"; chmod +x "$CONV_DIR/bin/claude"
CONV_BRAIN="$(bash -c ". '$HERE/conf.sh' >/dev/null 2>&1; printf %s \"\${SPIRA_WIKI:-\$SPIRA_REPO}\"")"
printf '%s\n%s\n' "$CONV_SID" "$CONV_BRAIN" > "$CONV_DIR/concierge-session"
bash -c "exec -a claude python3 -c 'import time; time.sleep(60)' --resume ${CONV_SID}" &
CONV_PID=$!
trap 'kill "$CONV_PID" 2>/dev/null; rm -rf "$CONV_DIR"; rm -rf "$TMP"' EXIT
sleep 0.3

CONV_SOCK="conv-no-second-$$"
conv_out="$(PATH="$CONV_DIR/bin:$PATH" SPIRA_RUN="$CONV_DIR" \
    CONCIERGE_SOCKET="$CONV_SOCK" CONCIERGE_SESSION="$CONV_SOCK" \
    bash "$HARNESS/concierge.sh" start 2>&1)"; conv_rc=$?
kill "$CONV_PID" 2>/dev/null; wait "$CONV_PID" 2>/dev/null
trap 'rm -rf "$TMP"' EXIT

# POSITIVE CONTROL: if the fixture holder was never found, none of the assertions below
# prove anything.
want "the fixture holder is detected"          "$CONV_SID"       "$conv_out"
if [ "$conv_rc" -ne 0 ]; then ok "a holder with no tmux session does not exit 0"; else bad "a holder with no tmux session does not exit 0" "exited 0"; fi
is   "start exits 3 (HEADLESS)"                              3 "$conv_rc"
want "it names the condition"                                "HEADLESS"        "$conv_out"
nowant "no claude stub was launched"                         "STUB_CLAUDE_RAN" "$conv_out"
nowant "it does not advise an attach that cannot work"       "attach:  tmux"   "$conv_out"

# POSITIVE CONTROL FOR THE GUARD ITSELF: without a live pid the convergence check is skipped
# and start proceeds to compose_brief. Pointed at an empty chamber, compose_brief fails with
# "no brief at..." — proving this is a fallthrough and not a second accidental short-circuit.
CONV_EMPTY="$(mktemp -d)"
conv_no_out="$(SPIRA_HOME="$CONV_EMPTY" SPIRA_RUN="$CONV_DIR" \
    CONCIERGE_SOCKET="$CONV_SOCK" CONCIERGE_SESSION="$CONV_SOCK" \
    bash "$HARNESS/concierge.sh" start 2>&1)"; conv_no_rc=$?
rm -rf "$CONV_EMPTY"
is   "start does not short-circuit without a live pid (proceeds to compose_brief)" 1 "$conv_no_rc"
want "and the failure is compose_brief's, not the convergence guard's" "no brief at" "$conv_no_out"

echo
echo "singleton — a bare, hand-started holder is detected by name, not by resume id (sp-rig42)"

# THE GAP concierge_live_pid DOESN'T COVER. That check finds a process holding the RECORDED
# resume id. A bare `claude --remote-control <name>` typed into a dead cockpit pane holds no
# resume id at all — it was never resumed — so it is invisible to that check even though it
# answers to the same Remote Control name and the same phone session. concierge_stray_holders
# scans for the NAME instead, via the internal _stray-holders subcommand.
SH_SESS="stray-test-$$"
SH_TMP="$TMP/stray"; mkdir -p "$SH_TMP"
SH_FAKE="$SH_TMP/fakeclaude"
printf '#!/bin/sh\nsleep 30\n' > "$SH_FAKE"; chmod +x "$SH_FAKE"

sh_holders() { SPIRA_RUN="$SH_TMP" SPIRA_WIKI="$SH_TMP" SPIRA_CONF="$TMP/no.conf" \
    CONCIERGE_SOCKET="$SH_SESS" CONCIERGE_SESSION="$SH_SESS" \
    bash "$HARNESS/concierge.sh" _stray-holders 2>/dev/null; }

# POSITIVE CONTROL: before any process registers under this made-up session name, none found.
is "no stray holders before one exists" "" "$(sh_holders)"

bash -c "exec -a claude-strayx '$SH_FAKE' --remote-control '$SH_SESS'" &
SH_PID=$!
trap 'kill "$SH_PID" 2>/dev/null; rm -rf "$TMP"' EXIT
sleep 0.3

is "the bare holder is found by name, with no resume id involved" "$SH_PID" "$(sh_holders)"

# start REFUSES rather than launching a third session on top of the confusion, and it does
# so before compose_brief — the statute book need not be present for this check to fire.
out_start="$(SPIRA_RUN="$SH_TMP" SPIRA_WIKI="$SH_TMP" SPIRA_CONF="$TMP/no.conf" \
    CONCIERGE_SOCKET="$SH_SESS" CONCIERGE_SESSION="$SH_SESS" \
    bash "$HARNESS/concierge.sh" start 2>&1)"; rc_start=$?
is   "start refuses (exit 4) when a stray holder is live" 4 "$rc_start"
want "and names the holding pid"                           "$SH_PID" "$out_start"

kill "$SH_PID" 2>/dev/null; wait "$SH_PID" 2>/dev/null || true
trap 'rm -rf "$TMP"' EXIT
is "no stray holders once the process exits" "" "$(sh_holders)"

echo
echo "wake — refuses rather than typing into a dead pane (sp-rig42)"

# has-session proves the tmux session exists; it says nothing about whether the process
# inside the pane is still alive. remain-on-exit keeps a dead pane around instead of tmux
# tearing the whole session down with it, which is what lets this be tested directly.
WK_SOCK="test-wake-dead-$$"
tmux -L "$WK_SOCK" kill-server 2>/dev/null || true
tmux -L "$WK_SOCK" new-session -d -s "$WK_SOCK" -x 80 -y 24
tmux -L "$WK_SOCK" set-option -t "$WK_SOCK" remain-on-exit on
tmux -L "$WK_SOCK" send-keys -t "$WK_SOCK" -l -- "exit" && tmux -L "$WK_SOCK" send-keys -t "$WK_SOCK" Enter
sleep 0.5

out_wake="$(CONCIERGE_SOCKET="$WK_SOCK" CONCIERGE_SESSION="$WK_SOCK" \
    bash "$HARNESS/concierge.sh" wake "hi" 2>&1)"; rc_wake=$?
is   "wake refuses on a dead pane"  1                      "$rc_wake"
want "and says why"                 "pane process has exited" "$out_wake"
tmux -L "$WK_SOCK" kill-server 2>/dev/null || true

# POSITIVE CONTROL: against a live pane, wake still succeeds — the refusal fires on
# deadness, not on every call.
WK_LIVE="test-wake-live-$$"
tmux -L "$WK_LIVE" kill-server 2>/dev/null || true
tmux -L "$WK_LIVE" new-session -d -s "$WK_LIVE" "sleep 30"
rc_wake_live=0
CONCIERGE_SOCKET="$WK_LIVE" CONCIERGE_SESSION="$WK_LIVE" \
    bash "$HARNESS/concierge.sh" wake "hi" >/dev/null 2>&1 || rc_wake_live=$?
is "wake succeeds against a live pane (positive control)" 0 "$rc_wake_live"
tmux -L "$WK_LIVE" kill-server 2>/dev/null || true

echo
echo "the way in — /proc scan hygiene and the dangling-resume retry, as source shape"

_src="$(cat "$HARNESS/concierge.sh")"
want "/proc read is guarded before it is attempted"            '[ -r "$f" ] || continue' "$_src"
want "redirect is grouped so the shell's own error is covered" '{ tr' "$_src"

want  "a failed start retries without --resume"     "retrying without it"                  "$_src"
want  "and clears the id that could not be resumed" 'rm -f "$SPIRA_RUN/concierge-session"' "$_src"
# The launcher is one line; grep -v deletes it entirely. Verify regeneration is used instead.
nowant "the retry regenerates rather than filtering the launcher" 'LAUNCHER.noresume' "$_src"

tl_summary
