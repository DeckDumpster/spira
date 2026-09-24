#!/usr/bin/env bash
#
# test-concierge-roster.sh — the concierge roster, its resume id, and the session hook that
#   records it: three seams named in section 5 of the operator-channel test plan, moved out
#   of the 760-line mixed test-concierge.sh into their own file with a fixture chamber.
#
# ROSTER (UC-operator-channel-39) — a fayth with FAYTH_SUMMON=operator never enters the task
# pool or the lanes. The shipped concierge is operator-only and the builder stays summonable.
# This carries its own positive control: an ordinary fayth sits in the same fixture chamber
# and MUST appear, because a test asserting only absence passes just as well against a roster
# that is empty for some unrelated reason (law-absence-needs-a-positive-control).
#
# RESUME ID (part of UC-operator-channel-40) — concierge_resume_id, tested through the
# internal `_resume-id` subcommand: the recorded session id is used only when the recorded
# cwd matches the brain dir.
#
# SESSION HOOK (part of UC-operator-channel-40) — hooks/session.sh records the concierge's
# own session id on context reset, gated on SPIRA_CONCIERGE=1 so no other brain session's
# context reset overwrites the file, and on SPIRA_PROD resolving to the same directory as
# SPIRA_HOME even through a symlink.
#
# No database, no systemd, no tmux, no live statute book. Under a second.
#
# defect: sp-u4x
# covers: spira/lib.sh concierge.sh spira/hooks/session.sh UC-operator-channel-39 UC-operator-channel-40
# hermetic-ok: fixture chamber, no systemd, tmux or database
# requires: claude
# host-reason: the resume-id and hook sections invoke concierge.sh, which refuses to run any subcommand without claude on PATH (spira_require)
# tier: T1
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
HARNESS="$(cd "$HERE/.." && pwd)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "the roster — who the sentinel may summon"

# A FIXTURE CHAMBER, NOT THE REAL ONE. The shipped $SPIRA_FAYTHS omits the concierge on every
# host today, so a suite reading the live roster would pass for a reason that has nothing to do
# with FAYTH_SUMMON — and would keep passing after the property was deleted.
CH="$TMP/chamber"; mkdir -p "$CH"
cat > "$CH/worker.fayth" <<'EOF'
FAYTH_NAME=worker
FAYTH_LABELS="spira,${SPIRA_PLAN_LABEL}"
EOF
cat > "$CH/laner.fayth" <<'EOF'
FAYTH_NAME=laner
FAYTH_LABELS="spira,${SPIRA_INCIDENT_LABEL}"
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
FAYTH_LABELS="spira,${SPIRA_INCIDENT_LABEL}"
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
echo "resume — the recorded session id is used only when the cwd still matches"

# SPIRA_WIKI is pinned to a non-default value so BRAIN inside concierge.sh is predictable
# and the session file can be written with the matching value.
FAKE_BRAIN="$TMP/fakebrain"; mkdir -p "$FAKE_BRAIN"
SID_FILE="$TMP/concierge-session"

resume_id() {
    SPIRA_RUN="$TMP" SPIRA_WIKI="$FAKE_BRAIN" SPIRA_CONF="$TMP/no.conf" \
        bash "$HARNESS/concierge.sh" _resume-id
}

# WITHOUT A SESSION FILE: no id returned (first run starts empty), but a diagnostic is printed
# so the operator can distinguish "no previous session" from "the recorder is broken".
rm -f "$SID_FILE"
rid="$(resume_id 2>"$TMP/err_absent")"
is   "no id when session file absent"            ""             "$rid"
want "and logs a diagnostic for the absent file" "starting fresh" "$(cat "$TMP/err_absent")"

# WITH A MATCHING CWD: the stored id is returned.
printf 'session-abc-123\n%s\n' "$FAKE_BRAIN" > "$SID_FILE"
rid="$(resume_id 2>/dev/null)"
is "id returned when cwd matches" "session-abc-123" "$rid"

# POSITIVE CONTROL: if the cwd field in the file is changed to something else, the id must
# NOT be returned — otherwise this test passes regardless of whether the check exists.
printf 'session-abc-123\n/old/path/that/differs\n' > "$SID_FILE"
rid="$(resume_id 2>"$TMP/err_mismatch")"
is   "empty when cwd mismatch"                "" "$rid"
want "and warns about the orphaned session" "starting empty" "$(cat "$TMP/err_mismatch")"

# Restore match so a re-read finds it again.
printf 'session-abc-123\n%s\n' "$FAKE_BRAIN" > "$SID_FILE"
is "id returned again after cwd is restored" "session-abc-123" "$(resume_id 2>/dev/null)"

echo
echo "session hook records the concierge session id on context reset"

# The hook fires with source=clear (or startup/compact) and a new session_id;
# SPIRA_CONCIERGE=1 gates recording.
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

# SEEN TO FAIL FIRST: without the flag the file must NOT be written — the positive control
# for every "records" assertion below.
rm -f "$SID_HOOK_DIR/run/concierge-session"
run_hook "hook-sid-noflag" "startup"
is "without SPIRA_CONCIERGE the session is not recorded" \
   "" "$(cat "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"

rm -f "$SID_HOOK_DIR/run/concierge-session"
run_hook "hook-sid-startup" "startup" SPIRA_CONCIERGE=1
is "startup with SPIRA_CONCIERGE=1 records the session id" \
   "hook-sid-startup" "$(sed -n '1p' "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"
is "and records the cwd on the second line" \
   "$SID_HOOK_DIR" "$(sed -n '2p' "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"

# CLEAR AND COMPACT BOTH UPDATE THE ID. Every clear, compact or fork mints a new session id.
run_hook "hook-sid-after-clear" "clear" SPIRA_CONCIERGE=1
is "/clear updates the recorded id to the new session id" \
   "hook-sid-after-clear" "$(sed -n '1p' "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"
run_hook "hook-sid-after-compact" "compact" SPIRA_CONCIERGE=1
is "/compact updates the recorded id" \
   "hook-sid-after-compact" "$(sed -n '1p' "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"

# A DIFFERENT SESSION WITH THE FLAG UNSET DOES NOT OVERWRITE. Other brain sessions fire the
# same global hook; only the concierge one has SPIRA_CONCIERGE=1.
run_hook "hook-sid-other-session" "startup"
is "another session without the flag does not overwrite" \
   "hook-sid-after-compact" "$(sed -n '1p' "$SID_HOOK_DIR/run/concierge-session" 2>/dev/null)"

echo
echo "session hook: SPIRA_PROD resolves through a symlink"

# SEEN TO FAIL FIRST: the in-force guard was a string comparison; a symlink path for
# SPIRA_PROD compared unequal to SPIRA_HOME (a resolved path) and the hook silently exited 0,
# recording nothing and leaving the concierge unable to resume after any session reset.
SID_LINK_DIR="$TMP/hookrun_link"; mkdir -p "$SID_LINK_DIR/run"
ln -sfn "$HERE" "$SID_LINK_DIR/spira-link"
HOOK_CONF_LINK="$SID_LINK_DIR/spira.conf"
cat > "$HOOK_CONF_LINK" <<EOF
SPIRA_PROD = $SID_LINK_DIR/spira-link
SPIRA_RUN = $SID_LINK_DIR/run
SPIRA_WATCHERS = $SID_LINK_DIR/no-watchers
EOF
: > "$SID_LINK_DIR/no-watchers"
run_hook_link() {
    local sid="$1" src="$2"; shift 2
    printf '{"hook_event_name":"SessionStart","session_id":"%s","source":"%s","cwd":"%s"}' \
        "$sid" "$src" "$SID_LINK_DIR" \
      | env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$HOOK_CONF_LINK" "$@" \
        bash "$HOOK" >/dev/null 2>&1
}
# NEGATIVE CONTROL: a symlink SPIRA_PROD pointing somewhere else must still refuse.
ln -sfn "$TMP" "$SID_LINK_DIR/spira-other"
HOOK_CONF_OTHER="$SID_LINK_DIR/spira-other.conf"
cat > "$HOOK_CONF_OTHER" <<EOF
SPIRA_PROD = $SID_LINK_DIR/spira-other
SPIRA_RUN = $SID_LINK_DIR/run
SPIRA_WATCHERS = $SID_LINK_DIR/no-watchers
EOF
rm -f "$SID_LINK_DIR/run/concierge-session"
printf '{"hook_event_name":"SessionStart","session_id":"%s","source":"%s","cwd":"%s"}' \
    "hook-sid-refused" "startup" "$SID_LINK_DIR" \
  | env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$HOOK_CONF_OTHER" SPIRA_CONCIERGE=1 \
    bash "$HOOK" >/dev/null 2>&1
is "symlink SPIRA_PROD to a different dir is still refused" \
   "" "$(cat "$SID_LINK_DIR/run/concierge-session" 2>/dev/null)"
# THE PROPERTY: a symlink SPIRA_PROD to the same dir as SPIRA_HOME must record.
rm -f "$SID_LINK_DIR/run/concierge-session"
run_hook_link "hook-sid-symlink" "startup" SPIRA_CONCIERGE=1
is "symlink SPIRA_PROD resolving to SPIRA_HOME records the session id" \
   "hook-sid-symlink" "$(sed -n '1p' "$SID_LINK_DIR/run/concierge-session" 2>/dev/null)"

tl_summary
