#!/usr/bin/env bash
#
# test-concierge.sh — the Concierge persona: composed like an aeon, summoned by nobody.
#
#   ./test-concierge.sh
#
# Two claims, and the second is the one with teeth.
#
# COMPOSED — concierge.sh renders chamber/concierge.md with every placeholder substituted and
# the statute book appended in full, and REFUSES to start when it cannot. A concierge launched
# without its brief looks identical to a working one from outside, and the way anybody finds
# out is the next violated statute.
#
# NEVER SUMMONED — the sentinel draws from spira_task_fayths and spira_lane_fayths, and
# neither may ever contain it. This one carries its own positive control: an ordinary fayth
# sits in the same fixture chamber and MUST appear, because a test asserting only absence
# passes just as well against a roster that is empty for some unrelated reason
# (law-absence-needs-a-positive-control).
#
# No database for the roster half, no network, under a second.
#
# defect: sp-u4x
# covers: spira/lib.sh concierge.sh systemd/concierge.service spira/bead.sh spira/chamber/concierge.fayth spira/chamber/concierge.md spira/hooks/session.sh spira/cockpit.sh
# hermetic-ok: fixture chamber, no systemd or database for the roster checks
# requires: claude
# host-reason: the brief section invokes concierge.sh which requires claude and tmux on PATH (operator tools not available in the container)
# scar: the concierge persona lacked FAYTH_SUMMON=operator and could be claimed by the sentinel as an ordinary worker.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
HARNESS="$(cd "$HERE/.." && pwd)"
pass=0; fail=0
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

is()     { if [ "$2" = "$3" ]; then pass=$((pass+1)); printf '  ok    %s\n' "$1"
           else fail=$((fail+1)); printf '  FAIL  %s: want [%s] got [%s]\n' "$1" "$2" "$3"; fi; }
want()   { case "$3" in *"$2"*) pass=$((pass+1)); printf '  ok    %s\n' "$1" ;;
           *) fail=$((fail+1)); printf '  FAIL  %s: wanted [%s]\n' "$1" "$2" ;; esac; }
nowant() { case "$3" in *"$2"*) fail=$((fail+1)); printf '  FAIL  %s: did not want [%s]\n' "$1" "$2" ;;
           *) pass=$((pass+1)); printf '  ok    %s\n' "$1" ;; esac; }

echo "the roster — who the sentinel may summon"

# A FIXTURE CHAMBER, NOT THE REAL ONE. The shipped $SPIRA_FAYTHS omits the concierge on every
# host today, so a suite reading the live roster would pass for a reason that has nothing to do
# with FAYTH_SUMMON — and would keep passing after the property was deleted.
CH="$TMP/chamber"; mkdir -p "$CH"
cat > "$CH/worker.fayth" <<'EOF'
FAYTH_NAME=worker
FAYTH_LABELS="spira,plan"
EOF
cat > "$CH/laner.fayth" <<'EOF'
FAYTH_NAME=laner
FAYTH_LABELS="spira,incident"
FAYTH_LANE=ops
EOF
cat > "$CH/human.fayth" <<'EOF'
FAYTH_NAME=human
FAYTH_SUMMON=operator
EOF
# A persona that is BOTH operator-summoned and in a lane. FAYTH_SUMMON must win: the lane
# loop is a second door into the sentinel, and a fayth kept out of one list and handed to the
# other is summoned exactly as if nothing had been declared.
cat > "$CH/humanlane.fayth" <<'EOF'
FAYTH_NAME=humanlane
FAYTH_LABELS="spira,incident"
FAYTH_LANE=ops
FAYTH_SUMMON=operator
EOF

roster() { # roster <function>
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$TMP" SPIRA_FAYTHS="worker laner human humanlane" \
        bash -c '. "$2"/lib.sh 2>/dev/null; "$1"' _ "$1" "$HERE" 2>/dev/null
}
# The fixture chamber must sit under SPIRA_HOME, which is where fayth_get and fayth_names look.
ln -sfn "$CH" "$TMP/chamber"

is "the task pool is the ordinary fayth alone"        "worker"          "$(roster spira_task_fayths)"
is "the lane list is the ordinary lane fayth alone"   "laner"           "$(roster spira_lane_fayths)"
# fayth_names sorts, so both operator personas appear in this order.
is "and the operator personas are named as such"      "human humanlane" "$(roster spira_operator_fayths)"

nowant "an operator persona is never in the task pool"  "human" "$(roster spira_task_fayths)"
nowant "nor in the lane list, which is the second door" "human" "$(roster spira_lane_fayths)"

# THE POSITIVE CONTROL FOR THE CONTROL. Strip FAYTH_SUMMON from the fixture and the same
# persona MUST appear — otherwise these assertions would pass against a roster that was empty
# for some unrelated reason, which is the shape of a test that guards nothing.
printf 'FAYTH_NAME=human\n' > "$CH/human.fayth"
want "without FAYTH_SUMMON that persona IS summoned" "human" "$(roster spira_task_fayths)"

echo
echo "the shipped concierge — real persona, summoned by nobody"

ship() { # ship <function>   — the REAL chamber, with the concierge listed in the roster
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$HERE" SPIRA_FAYTHS="builder ops concierge" \
        bash -c '. "$2"/lib.sh 2>/dev/null; "$1"' _ "$1" "$HERE" 2>/dev/null
}
# LISTED IN $SPIRA_FAYTHS ON PURPOSE. No host lists it today, so the roster alone would keep
# it out — and that is a second reason, not the one under test. Naming it here removes the
# reason that is doing the work by accident and leaves only FAYTH_SUMMON holding the line.
nowant "the shipped concierge is not in the task pool"   "concierge" "$(ship spira_task_fayths)"
nowant "the shipped concierge is not in the lane list"   "concierge" "$(ship spira_lane_fayths)"
want   "the shipped concierge IS an operator persona"    "concierge" "$(ship spira_operator_fayths)"
want   "and the other personas are still summonable"     "builder"   "$(ship spira_task_fayths)"

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
    pass=$((pass+1)); printf '  ok    brief without args does not exit 2\n'
else
    fail=$((fail+1)); printf '  FAIL  brief without args exited 2 — positive control broken\n'
fi

echo
echo "resume — session continuity"

# concierge_resume_id is tested through the internal _resume-id subcommand.
# SPIRA_WIKI is pinned to a non-default value so BRAIN inside concierge.sh is predictable
# and the session file can be written with the matching value.
FAKE_BRAIN="$TMP/fakebrain"; mkdir -p "$FAKE_BRAIN"
SID_FILE="$TMP/concierge-session"

# WITHOUT A SESSION FILE: no id returned (first run starts empty).
rm -f "$SID_FILE"
rid="$(SPIRA_RUN="$TMP" SPIRA_WIKI="$FAKE_BRAIN" SPIRA_CONF="$TMP/no.conf" \
    bash "$HARNESS/concierge.sh" _resume-id 2>/dev/null)"
is "no id when session file absent"    ""  "$rid"

# WITH A MATCHING CWD: the stored id is returned.
printf 'session-abc-123\n%s\n' "$FAKE_BRAIN" > "$SID_FILE"
rid="$(SPIRA_RUN="$TMP" SPIRA_WIKI="$FAKE_BRAIN" SPIRA_CONF="$TMP/no.conf" \
    bash "$HARNESS/concierge.sh" _resume-id 2>/dev/null)"
is "id returned when cwd matches"      "session-abc-123"  "$rid"

# POSITIVE CONTROL: if the cwd field in the file is changed to something else, the id must
# NOT be returned — otherwise this test passes regardless of whether the check exists.
printf 'session-abc-123\n/old/path/that/differs\n' > "$SID_FILE"
rid="$(SPIRA_RUN="$TMP" SPIRA_WIKI="$FAKE_BRAIN" SPIRA_CONF="$TMP/no.conf" \
    bash "$HARNESS/concierge.sh" _resume-id 2>"$TMP/err_resume")"
is "empty when cwd mismatch"           ""  "$rid"
want "and warns about the orphaned session" "starting empty" "$(cat "$TMP/err_resume")"

# Restore match so subsequent tests get a clean file.
printf 'session-abc-123\n%s\n' "$FAKE_BRAIN" > "$SID_FILE"
rid2="$(SPIRA_RUN="$TMP" SPIRA_WIKI="$FAKE_BRAIN" SPIRA_CONF="$TMP/no.conf" \
    bash "$HARNESS/concierge.sh" _resume-id 2>/dev/null)"
is "id returned again after cwd is restored"  "session-abc-123"  "$rid2"

echo
echo "the brief — composed, or refused"

# Brief rendering requires statutes in the rule database. A fresh container has none,
# so these tests skip rather than fail when rule.sh list returns nothing. The assertions
# run in full on an operator host where the statute book is populated.
if ! bash "$HARNESS/rule.sh" list 2>/dev/null | grep -q .; then
    printf '  skip  (brief: statute book is empty in this environment — skipping brief tests)\n'
else

BRIEF="$(bash "$HARNESS/concierge.sh" brief 2>"$TMP/err")"
if [ -n "$BRIEF" ] && [ -f "$BRIEF" ]; then
    pass=$((pass+1)); printf '  ok    concierge.sh brief renders a file\n'
    B="$(cat "$BRIEF")"
    # EVERY PLACEHOLDER, because an unsubstituted one is a command line the session will try
    # to run. The failure arrives hours later as "the concierge does not escalate anything".
    nowant "no placeholder survives rendering"  "{{"          "$B"
    want   "the brief names the mail path"      "mail.sh send operator"  "$B"
    mail_path="$(printf '%s\n' "$B" | grep -oE '[^ `]+mail\.sh' | head -1)"
    if [ -n "$mail_path" ] && [ -x "$mail_path" ]; then
        pass=$((pass+1)); printf '  ok    mail path in brief exists and is executable: %s\n' "$mail_path"
    else
        fail=$((fail+1)); printf '  FAIL  mail path in brief does not exist or is not executable: [%s]\n' "${mail_path:-<not found>}"
    fi
    want   "and the bead contract"              "bead.sh file" "$B"
    # THE FILE, NOT THE STRING. The string check above is the positive control: the path
    # must be named for the grep below to find it. The assertion with teeth is this one —
    # a path that is named but absent passes the string check and fails here.
    bead_path="$(printf '%s\n' "$B" | grep -oE '[^ ]+bead\.sh' | head -1)"
    if [ -n "$bead_path" ] && [ -x "$bead_path" ]; then
        pass=$((pass+1)); printf '  ok    bead tool path in brief exists and is executable: %s\n' "$bead_path"
    else
        fail=$((fail+1)); printf '  FAIL  bead tool path in brief does not exist or is not executable: [%s]\n' "${bead_path:-<not found>}"
    fi
    want   "and carries the statute book"       "# Memories in force" "$B"
    # THE STATUTES THIS ROLE IS ACTUALLY HELD TO, IN FULL TEXT — not as index slugs. This is
    # the whole reason FAYTH_STATUTE_CORE exists: the shipped core set is builder-shaped, and
    # the statute the operator's own session violated for 39 turns was outside it.
    for law in law-the-harness-checkout-is-production law-filed-bead-queued-xor-escalated \
               law-decisions-surface-immediately law-closed-is-not-landed; do
        want "  $law is rendered in full" "## $law" "$B"
    done
else
    fail=$((fail+1)); printf '  FAIL  concierge.sh brief produced nothing:\n%s\n' "$(cat "$TMP/err")"
fi

# A MISSING BRIEF IS A REFUSAL, NOT A DEGRADED START — the assertion that the refusal exists.
# Pointed at a persona with no markdown, it must fail loudly rather than launch a session whose
# only difference from a working one is that it was never told anything.
out="$(CONCIERGE_FAYTH=no-such-persona bash "$HARNESS/concierge.sh" brief 2>&1)"; rc=$?
is   "a missing brief exits non-zero"     1 "$rc"
want "and says which file was missing"    "no-such-persona.md" "$out"

# A CORE SET THAT RENDERS NOTHING IN FULL IS A TYPO, AND IT IS THE SILENT ONE. render_memories
# matches core slugs EXACTLY and demotes anything it does not recognise to the index tier
# without a word, so a mistyped or retired slug costs that statute its full text and says
# nothing at all. The brief still looks complete — right size, every placeholder filled, the
# law apparently present — which is why this needs an assertion rather than a reader.
#
# A FIXTURE PERSONA, NOT THE SHIPPED ONE. Editing chamber/concierge.fayth to drive this would
# leave the suite one failed assertion away from having corrupted the thing it tests.
FX="$TMP/fx"; mkdir -p "$FX/chamber"
cp "$HERE/chamber/concierge.md" "$FX/chamber/typo.md"
sed -e 's|^FAYTH_STATUTE_CORE=.*|FAYTH_STATUTE_CORE="law-slug-that-does-not-exist"|' \
    -e 's|^FAYTH_NAME=.*|FAYTH_NAME=typo|' \
    "$HERE/chamber/concierge.fayth" > "$FX/chamber/typo.fayth"
# The launcher reads $SPIRA_HOME/chamber, so the fixture chamber has to be the one it finds.
# Everything else about the harness stays real: the point is that the STATUTE lookup misses.
out="$(SPIRA_HOME="$FX" CONCIERGE_FAYTH=typo bash "$HARNESS/concierge.sh" brief 2>&1)"; rc=$?
is   "an all-typo core set exits non-zero"  1 "$rc"
want "and says the slugs were demoted"      "no statute rendered in full" "$out"

# THE POSITIVE CONTROL FOR THAT REFUSAL. The same fixture with ONE real slug must compose —
# otherwise the assertion above would pass against a fixture that was broken for some
# unrelated reason, which is most of them (law-absence-needs-a-positive-control).
sed -i 's|^FAYTH_STATUTE_CORE=.*|FAYTH_STATUTE_CORE="law-closed-is-not-landed"|' "$FX/chamber/typo.fayth"
out="$(SPIRA_HOME="$FX" CONCIERGE_FAYTH=typo bash "$HARNESS/concierge.sh" brief 2>&1)"; rc=$?
is   "one real slug composes a brief"       0 "$rc"
want "and renders that statute in full"     "## law-closed-is-not-landed" "$(cat "$out" 2>/dev/null)"

# THE SUMMARY LINE IS READ BY THE OPERATOR AND BY NOBODY ELSE, which is exactly why it needs
# an assertion: `here` grew its own inline count, the quoting came out wrong, and it reported
# "0 statutes in full" about a brief holding twenty. A launcher reporting on itself is the
# reading nobody goes behind.
#
# AND IT IS PARSED, NOT SUBSTRING-MATCHED. The first version of this check asserted that the
# line does not CONTAIN "0 statutes" and failed against the correct output, because "20
# statutes" contains it. A count is a number; compare it as one.
sum="$(bash "$HARNESS/concierge.sh" here --help 2>&1 >/dev/null | head -1)"
want "the summary names a count and a size" "statutes in full," "$sum"
n_sum="$(sed -n 's/^concierge: \([0-9]*\) statutes in full.*/\1/p' <<<"$sum")"
if [ -n "$n_sum" ] && [ "$n_sum" -gt 0 ] 2>/dev/null; then
    pass=$((pass+1)); printf '  ok    and the count is %s, not zero\n' "$n_sum"
else
    fail=$((fail+1)); printf '  FAIL  the summary reported [%s] statutes: %s\n' "$n_sum" "$sum"
fi
# THE COUNT MUST BE THE BRIEF'S OWN, not a constant that happens to look plausible.
is "and it matches the rendered brief" \
   "$(grep -c '^## law-' "$(bash "$HARNESS/concierge.sh" brief)" 2>/dev/null)" "$n_sum"


echo
echo "the brief — no-wiki install (SPIRA_WIKI unset)"

# SPIRA_WIKI UNSET: every path the brief names must be a file that exists on this host.
# The filing tool ships in the harness (spira/bead.sh) and is referenced via {{BEAD}}, so
# its rendered path must exist regardless of whether a wiki is configured.
# THE POSITIVE CONTROL: the brief must still name the bead tool; absence of .claude/bead.sh
# alone would pass just as well against a brief that named nothing at all.
BRIEF_NW="$(SPIRA_WIKI= bash "$HARNESS/concierge.sh" brief 2>"$TMP/err_nw")"
if [ -n "$BRIEF_NW" ] && [ -f "$BRIEF_NW" ]; then
    pass=$((pass+1)); printf '  ok    no-wiki brief renders\n'
    BNW="$(cat "$BRIEF_NW")"
    nowant "no-wiki brief does not name the wiki-relative tool"  ".claude/bead.sh" "$BNW"
    want   "no-wiki brief still names the harness bead tool"     "bead.sh file"    "$BNW"
    bead_path="$(printf '%s\n' "$BNW" | grep -oE '[^ ]+bead\.sh' | head -1)"
    if [ -n "$bead_path" ] && [ -f "$bead_path" ]; then
        pass=$((pass+1)); printf '  ok    bead tool path in brief exists: %s\n' "$bead_path"
    else
        fail=$((fail+1)); printf '  FAIL  bead tool path in brief does not exist: [%s]\n' "${bead_path:-<not found>}"
    fi
else
    fail=$((fail+1)); printf '  FAIL  no-wiki brief failed:\n%s\n' "$(cat "$TMP/err_nw")"
fi

echo
echo "start — session survives the oneshot's cgroup teardown"

# THE PROPERTY UNDER TEST. concierge.service is Type=oneshot/KillMode=control-group:
# the server shared the service's cgroup and died when start exited. We simulate the
# oneshot with systemd-run --wait (which kills its cgroup on exit) and assert the
# session still exists. Without the fix, the session dies with the cgroup.
if ! systemctl --user status >/dev/null 2>&1 || ! command -v systemd-run >/dev/null 2>&1; then
    printf '  skip  (no systemd user session — cgroup survival test requires it)\n'
else
    SOCK="test-concierge-$$"
    tmux -L "$SOCK" kill-server 2>/dev/null || true  # clear any leftover from a prior run

    rc_start=0
    systemd-run --user --wait --collect --quiet -- \
        env CONCIERGE_SOCKET="$SOCK" CONCIERGE_SESSION="$SOCK" \
        bash "$HARNESS/concierge.sh" start 2>>"$TMP/err" || rc_start=$?

    is "start exits 0 under a simulated oneshot" 0 "$rc_start"

    alive=0
    tmux -L "$SOCK" has-session -t "$SOCK" 2>/dev/null && alive=1
    is "session survives after the oneshot cgroup is torn down" 1 "$alive"

    # THE POSITIVE CONTROL: if start never started tmux (e.g. compose_brief failed),
    # alive would be 0 for a different reason. rc_start catches that case above.

    tmux -L "$SOCK" kill-server 2>/dev/null || true  # drains the nested transient unit
fi

echo
echo "resume — launcher carries the resume flag"

# THE PROPERTY UNDER TEST. When $SPIRA_RUN/concierge-session holds an id whose cwd matches
# BRAIN, the launcher script must carry --resume <id>. Without the recorded id it must not.
#
# POSITIVE CONTROL: a launcher with no session file MUST NOT carry --resume, so if the check
# were absent the "resume present" assertion below would pass against a launcher that adds it
# unconditionally.
if ! systemctl --user status >/dev/null 2>&1 || ! command -v systemd-run >/dev/null 2>&1; then
    printf '  skip  (no systemd user session — launcher resume test requires it)\n'
else
    TMP_RUN="$TMP/run-resume"; mkdir -p "$TMP_RUN"
    FAKE_SID_R="resume-launcher-test-$(date +%s)"

    # First: no session file → launcher must NOT carry --resume.
    rm -f "$TMP_RUN/concierge-session"
    SOCK_NR="test-concierge-noresume-$$"
    tmux -L "$SOCK_NR" kill-server 2>/dev/null || true
    SPIRA_RUN="$TMP_RUN" CONCIERGE_SOCKET="$SOCK_NR" CONCIERGE_SESSION="$SOCK_NR" \
        systemd-run --user --wait --collect --quiet -- \
        env SPIRA_RUN="$TMP_RUN" CONCIERGE_SOCKET="$SOCK_NR" CONCIERGE_SESSION="$SOCK_NR" \
        bash "$HARNESS/concierge.sh" start 2>>"$TMP/err" || true
    tmux -L "$SOCK_NR" kill-server 2>/dev/null || true
    if [ -f "$TMP_RUN/concierge-launch.sh" ]; then
        nowant "launcher has no --resume when no session file" "--resume" \
            "$(cat "$TMP_RUN/concierge-launch.sh")"
    else
        fail=$((fail+1)); printf '  FAIL  start did not write launcher (no-session case)\n'
    fi

    # Second: session file with matching cwd → launcher must carry --resume <id>.
    # BRAIN inside concierge.sh is SPIRA_WIKI when set, so we pin it to TMP_RUN for a
    # predictable, non-default value that the session file can match.
    printf '%s\n%s\n' "$FAKE_SID_R" "$TMP_RUN" > "$TMP_RUN/concierge-session"
    SOCK_R="test-concierge-resume2-$$"
    tmux -L "$SOCK_R" kill-server 2>/dev/null || true
    SPIRA_RUN="$TMP_RUN" SPIRA_WIKI="$TMP_RUN" CONCIERGE_SOCKET="$SOCK_R" CONCIERGE_SESSION="$SOCK_R" \
        systemd-run --user --wait --collect --quiet -- \
        env SPIRA_RUN="$TMP_RUN" SPIRA_WIKI="$TMP_RUN" \
            CONCIERGE_SOCKET="$SOCK_R" CONCIERGE_SESSION="$SOCK_R" \
        bash "$HARNESS/concierge.sh" start 2>>"$TMP/err" || true
    tmux -L "$SOCK_R" kill-server 2>/dev/null || true
    if [ -f "$TMP_RUN/concierge-launch.sh" ]; then
        lnch="$(cat "$TMP_RUN/concierge-launch.sh")"
        want "launcher carries --resume when session file exists" "--resume"    "$lnch"
        want "launcher carries the specific session id"           "$FAKE_SID_R" "$lnch"
    else
        fail=$((fail+1)); printf '  FAIL  start did not write launcher (resume case)\n'
    fi
fi

fi  # statute book guard

echo
echo "the ensure unit leaves the tmux server it starts alive"
svc="$(sed -n '/^\[Service\]/,/^\[/p' "$HARNESS/systemd/concierge.service" | grep -v '^\s*#')"
want "the unit is a oneshot, whose cgroup is reaped when start returns" "Type=oneshot" "$svc"
# THE MECHANISM IS IN THE SCRIPT, not the unit: concierge.sh start wraps tmux new-session
# with systemd-run --remain-after-exit, putting the server in its own transient cgroup that
# outlives the oneshot. A KillMode=process guard in the unit file was the previous approach;
# this assert confirms the mechanism is visible in the script where maintainers look.
want "start escapes the oneshot cgroup via systemd-run --remain-after-exit" \
    "remain-after-exit" "$(cat "$HARNESS/concierge.sh")"

echo
echo "session hook records the concierge session id on context reset"

# (a) /clear inside the concierge updates the recorded id.
# The hook fires with source=clear and a new session_id; SPIRA_CONCIERGE=1 gates recording.
#
# SEEN TO FAIL FIRST: without SPIRA_CONCIERGE=1 the hook must NOT record, proving the guard
# exists rather than the write being unconditional.
SID_HOOK_DIR="$TMP/hookrun"; mkdir -p "$SID_HOOK_DIR"
HOOK="$HERE/hooks/session.sh"

# Build a minimal conf the hook can source. SPIRA_PROD must match SPIRA_HOME so the
# in-force guard passes; everything else is moved to non-defaults.
HOOK_CONF="$SID_HOOK_DIR/spira.conf"
cat > "$HOOK_CONF" <<EOF
SPIRA_PROD = $HERE
SPIRA_RUN = $SID_HOOK_DIR/run
SPIRA_WATCHERS = $SID_HOOK_DIR/no-watchers
EOF
mkdir -p "$SID_HOOK_DIR/run"
: > "$SID_HOOK_DIR/no-watchers"

run_hook() {  # run_hook <session_id> <source> [extra-env...]
    local sid="$1" src="$2"; shift 2
    printf '{"hook_event_name":"SessionStart","session_id":"%s","source":"%s","cwd":"%s"}' \
        "$sid" "$src" "$SID_HOOK_DIR" \
      | env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$HOOK_CONF" "$@" \
        bash "$HOOK" >/dev/null 2>&1
}

# POSITIVE CONTROL: without the flag the file must NOT be written.
rm -f "$SID_HOOK_DIR/run/concierge-session"
run_hook "hook-sid-noflag" "startup"
is "without SPIRA_CONCIERGE the session is not recorded" \
   "" "$(cat "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"

# THE PROPERTY UNDER TEST: with SPIRA_CONCIERGE=1 the file IS written.
rm -f "$SID_HOOK_DIR/run/concierge-session"
run_hook "hook-sid-startup" "startup" SPIRA_CONCIERGE=1
is "startup with SPIRA_CONCIERGE=1 records the session id" \
   "hook-sid-startup" "$(sed -n '1p' "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"
is "and records the cwd on the second line" \
   "$SID_HOOK_DIR" "$(sed -n '2p' "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"

# CLEAR UPDATES THE ID. After /clear the session id changes; the hook fires with the new one.
run_hook "hook-sid-after-clear" "clear" SPIRA_CONCIERGE=1
is "/clear updates the recorded id to the new session id" \
   "hook-sid-after-clear" "$(sed -n '1p' "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"

# COMPACT UPDATES IT TOO.
run_hook "hook-sid-after-compact" "compact" SPIRA_CONCIERGE=1
is "/compact updates the recorded id" \
   "hook-sid-after-compact" "$(sed -n '1p' "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"

# A DIFFERENT SESSION WITH THE FLAG UNSET DOES NOT OVERWRITE. Other brain sessions fire the
# same global hook; only the concierge one has SPIRA_CONCIERGE=1.
run_hook "hook-sid-other-session" "startup"
is "another session without the flag does not overwrite" \
   "hook-sid-after-compact" "$(sed -n '1p' "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"

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
bash -c "exec -a claude-resume-${LP_SID} sleep 10" &
LP_PID=$!
trap 'kill "$LP_PID" 2>/dev/null; rm -rf "$TMP"' EXIT

found="$(lp "$LP_SID")" || found=""
is "live pid is found when a process holds the id" "$LP_PID" "$found"

# PROCESS GONE: pid is no longer returned after the process exits.
kill "$LP_PID" 2>/dev/null; wait "$LP_PID" 2>/dev/null || true
is "pid is gone after the process exits" "" "$(lp "$LP_SID")"
trap 'rm -rf "$TMP"' EXIT  # restore trap without the kill

echo
echo "cockpit.sh attaches the operator (acceptance c)"

# (c) cockpit.sh after killing the concierge tmux server comes back on the same session id.
# SPIRA_COCKPIT_NO_ATTACH=1 prevents the exec so the test can inspect what would have happened.
# SEEN TO FAIL FIRST: with the flag clear and no TMUX set, cockpit.sh calls concierge.sh start
# then attach. We verify start writes the right resume id.
#
# This requires systemd and claude, so it runs only where both are present.
if ! systemctl --user status >/dev/null 2>&1 || ! command -v systemd-run >/dev/null 2>&1 \
   || ! bash "$HARNESS/rule.sh" list 2>/dev/null | grep -q .; then
    printf '  skip  (cockpit attach: requires systemd user session and statute book)\n'
else

CC_TMP="$TMP/cockpit-attach-test"; mkdir -p "$CC_TMP"
CC_SID="cockpit-attach-$(date +%s)"
CC_SOCK="test-cockpit-attach-$$"
printf '%s\n%s\n' "$CC_SID" "$CC_TMP" > "$CC_TMP/concierge-session"

# POSITIVE CONTROL: without the session file the launcher must not carry --resume.
# This confirms that when a resume id IS present the guard is doing real work.
TMP_NR="$CC_TMP/no-resume"; mkdir -p "$TMP_NR"
tmux -L "$CC_SOCK-nr" kill-server 2>/dev/null || true
SPIRA_RUN="$TMP_NR" SPIRA_WIKI="$CC_TMP" \
    CONCIERGE_SOCKET="$CC_SOCK-nr" CONCIERGE_SESSION="$CC_SOCK-nr" \
    systemd-run --user --wait --collect --quiet -- \
    env SPIRA_RUN="$TMP_NR" SPIRA_WIKI="$CC_TMP" \
        CONCIERGE_SOCKET="$CC_SOCK-nr" CONCIERGE_SESSION="$CC_SOCK-nr" \
    bash "$HARNESS/concierge.sh" start 2>/dev/null || true
tmux -L "$CC_SOCK-nr" kill-server 2>/dev/null || true
if [ -f "$TMP_NR/concierge-launch.sh" ]; then
    nowant "launcher without session file has no --resume" "--resume" \
        "$(cat "$TMP_NR/concierge-launch.sh")"
fi

# THE PROPERTY: with the session file, the new launcher carries --resume <CC_SID>.
tmux -L "$CC_SOCK" kill-server 2>/dev/null || true
SPIRA_RUN="$CC_TMP" SPIRA_WIKI="$CC_TMP" \
    CONCIERGE_SOCKET="$CC_SOCK" CONCIERGE_SESSION="$CC_SOCK" \
    systemd-run --user --wait --collect --quiet -- \
    env SPIRA_RUN="$CC_TMP" SPIRA_WIKI="$CC_TMP" \
        CONCIERGE_SOCKET="$CC_SOCK" CONCIERGE_SESSION="$CC_SOCK" \
    bash "$HARNESS/concierge.sh" start 2>/dev/null || true
tmux -L "$CC_SOCK" kill-server 2>/dev/null || true
if [ -f "$CC_TMP/concierge-launch.sh" ]; then
    lnch="$(cat "$CC_TMP/concierge-launch.sh")"
    want "after cockpit kills and restarts, launcher carries --resume" "--resume" "$lnch"
    want "and names the recorded session id"  "$CC_SID" "$lnch"
    want "and the launcher exports SPIRA_CONCIERGE=1" "SPIRA_CONCIERGE=1" "$lnch"
else
    fail=$((fail+1)); printf '  FAIL  start did not write launcher (cockpit-attach case)\n'
fi

fi  # systemd guard

echo
echo "convergence — start and here attach to the live session (acceptance b)"

# (b) start, here, and cockpit.sh while one process holds the id produce no second claude.
# A mock tmux records every call: TMUX_LOG captures all subcommand args.
# SEEN TO FAIL FIRST: old code printed "refusing" and exited 1; new code prints "converging"
# and calls tmux attach.

CONV_TMP="$TMP/conv"; mkdir -p "$CONV_TMP/bin"
CONV_SID="converge-test-$(date +%s)"
CONV_SOCK="test-conv-$$"
printf '%s\n%s\n' "$CONV_SID" "$CONV_TMP" > "$CONV_TMP/concierge-session"

TMUX_LOG="$CONV_TMP/tmux.log"
cat > "$CONV_TMP/bin/tmux" <<'TMUXEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$TMUX_LOG"
case " $* " in
    *" has-session "*)   exit 0 ;;
    *" attach "*)        exit 0 ;;
    *" kill-server"*)    exit 0 ;;
    *) /usr/bin/tmux "$@" 2>/dev/null; exit $? ;;
esac
TMUXEOF
chmod +x "$CONV_TMP/bin/tmux"

# POSITIVE CONTROL: no live process → start must NOT converge; tmux attach must NOT appear.
# SPIRA_PATH prepends the mock directory so conf.sh's PATH reset still finds the mock tmux.
TMUX_LOG="$CONV_TMP/tmux-no-proc.log" SPIRA_PATH="$CONV_TMP/bin" \
    PATH="$CONV_TMP/bin:$PATH" SPIRA_RUN="$CONV_TMP" SPIRA_WIKI="$CONV_TMP" \
    CONCIERGE_SOCKET="$CONV_SOCK" CONCIERGE_SESSION="$CONV_SOCK" SPIRA_CONF="$TMP/no.conf" \
    bash "$HARNESS/concierge.sh" start 2>/dev/null 1>/dev/null || true
nowant "without a live process start does not converge" "attach" \
    "$(cat "$CONV_TMP/tmux-no-proc.log" 2>/dev/null)"

# THE PROPERTY: live process holds the id → start converges.
bash -c "exec -a claude-resume-${CONV_SID} sleep 30" &
CONV_PID=$!
trap 'kill "$CONV_PID" 2>/dev/null; rm -rf "$TMP"' EXIT

CONV_OUT=""
CONV_OUT="$(TMUX_LOG="$CONV_TMP/tmux.log" SPIRA_PATH="$CONV_TMP/bin" \
    PATH="$CONV_TMP/bin:$PATH" SPIRA_RUN="$CONV_TMP" SPIRA_WIKI="$CONV_TMP" \
    CONCIERGE_SOCKET="$CONV_SOCK" CONCIERGE_SESSION="$CONV_SOCK" SPIRA_CONF="$TMP/no.conf" \
    bash "$HARNESS/concierge.sh" start 2>&1)"
nowant "start with live pid does not say 'refusing'"  "refusing"   "$CONV_OUT"
want   "start with live pid says 'converging'"        "converging" "$CONV_OUT"
want   "and tmux attach was called"                   "attach"     \
    "$(cat "$CONV_TMP/tmux.log" 2>/dev/null)"

# `here` convergence: live pid → attach, never a second claude.
# Convergence in `here` is checked before compose_brief, so it runs even without a
# statute book. SPIRA_PATH ensures the mock tmux survives conf.sh's PATH reset.
: > "$CONV_TMP/tmux-here.log"
HERE_OUT=""
HERE_OUT="$(TMUX_LOG="$CONV_TMP/tmux-here.log" SPIRA_PATH="$CONV_TMP/bin" \
    PATH="$CONV_TMP/bin:$PATH" SPIRA_RUN="$CONV_TMP" SPIRA_WIKI="$CONV_TMP" \
    CONCIERGE_SOCKET="$CONV_SOCK" CONCIERGE_SESSION="$CONV_SOCK" SPIRA_CONF="$TMP/no.conf" \
    bash "$HARNESS/concierge.sh" here 2>&1)"
nowant "here with live pid does not say 'refusing'"  "refusing"   "$HERE_OUT"
want   "here with live pid says 'converging'"        "converging" "$HERE_OUT"
want   "and here called tmux attach"                 "attach"     \
    "$(cat "$CONV_TMP/tmux-here.log" 2>/dev/null)"

kill "$CONV_PID" 2>/dev/null; wait "$CONV_PID" 2>/dev/null || true
trap 'rm -rf "$TMP"' EXIT

echo
echo "cockpit.sh from the session pane shows the concierge (acceptance d)"

# (d) the cockpit session pane shows the concierge after cockpit.sh from nothing.
# The old behaviour: TMUX set caused "cannot attach from inside a tmux pane" and exit 1.
# The new behaviour: TMUX set causes a nested attach via concierge.sh attach.
#
# A mock concierge.sh records what subcommands were called.
# SEEN TO FAIL FIRST: the old code exited 1 with "cannot attach" when TMUX was set.
# After the fix the mock concierge.sh is called with "attach" and cockpit.sh exits 0.

CPANE_TMP="$TMP/cpane"; mkdir -p "$CPANE_TMP"
CPANE_LOG="$CPANE_TMP/conc.log"
cat > "$CPANE_TMP/concierge.sh" <<'CONCEOF'
#!/usr/bin/env bash
printf 'concierge.sh %s\n' "$1" >> "$CONC_LOG"
case "${1:-}" in
    status) exit 0 ;;
    attach) exit 0 ;;
    *)      exit 0 ;;
esac
CONCEOF
chmod +x "$CPANE_TMP/concierge.sh"

# Build a minimal conf for cockpit.sh to source. SPIRA_PROD must match SPIRA_HOME so the
# in-force guard passes. SPIRA_REPO cannot be set via conf file (deliberately absent from
# conf.sh's settable list), so it is passed as an env var instead.
CPANE_CONF="$CPANE_TMP/spira.conf"
cat > "$CPANE_CONF" <<EOF
SPIRA_PROD = $HERE
SPIRA_RUN = $CPANE_TMP/run
SPIRA_WATCHERS = $CPANE_TMP/no-watchers
EOF
mkdir -p "$CPANE_TMP/run"
: > "$CPANE_TMP/no-watchers"

# POSITIVE CONTROL (seen to fail with old code): TMUX set must NOT produce
# "cannot attach" with the new cockpit.sh. The old code exited 1 here.
# SPIRA_REPO cannot be set via conf file, so pass it as env var so cockpit.sh
# finds the mock concierge.sh instead of the real one.
# SPIRA_IN_TESTENV= clears the batch flag. SPIRA_INSTANCE= resets the per-suite
# instance (e.g. 7eaac62db8b4-1) injected in parallel mode — spira_containment_check
# exits 1 when SPIRA_INSTANCE is non-prod and repo-map entries fall outside SPIRA_WORKSPACES.
CPANE_OUT=""; CPANE_RC=0
CPANE_OUT="$(TMUX=fake_tmux_pane CONC_LOG="$CPANE_LOG" SPIRA_CONF="$CPANE_CONF" \
    SPIRA_REPO="$CPANE_TMP" SPIRA_IN_TESTENV= SPIRA_INSTANCE= bash "$HERE/cockpit.sh" 2>&1)" || CPANE_RC=$?
nowant "cockpit.sh from pane does not say 'cannot attach'" "cannot attach" "$CPANE_OUT"
is    "cockpit.sh from pane exits 0"                       0               "$CPANE_RC"
want  "cockpit.sh from pane calls concierge.sh attach"     "attach"        \
    "$(cat "$CPANE_LOG" 2>/dev/null)"

echo
echo "the launcher exports SPIRA_CONCIERGE=1"
# Verify the launcher the test-startup case wrote (from the "resume — launcher carries the
# resume flag" section) also exports the concierge flag, without re-running start.
# We check the script text directly: if the launcher was written in the resume section above
# it has it; if not we check the source to verify the line is present.
want "the launcher source contains SPIRA_CONCIERGE export" \
    "SPIRA_CONCIERGE=1" "$(cat "$HARNESS/concierge.sh")"

echo
echo "concierge self-test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
