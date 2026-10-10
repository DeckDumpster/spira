#!/usr/bin/env bash
# tier: T2
# covers: systemd/spira-mail-deliver.service spira-world/src/bin/world.rs watchd/* spira/watchers spira/spira-mail-deliver.sh mail/src/* spira/mail-health.sh spira/conf.sh UC-operator-channel-10
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. SERVICE UNIT: SuccessExitStatus=143 is set — a halt's SIGTERM exit is clean (inactive,
#    not failed), so world.sh start can revive the unit.
# 2. WORLD.SH START: behavioural — against a mocked systemctl, `start` revives
#    spira-mail-deliver when it is enabled-but-inactive, leaves a disabled unit alone, and
#    does not restart an already-active one (coverage-map row 10, SOURCE-GREP: a source
#    grep proved the code MENTIONS these rules; running `start` proves it OBEYS them).
# 3. LIVENESS: watchd's extern health probe is daemon liveness only — is the daemon active,
#    and is it watching every registered mailbox. Inactive escalates unconditionally now;
#    active-but-not-watching a mailbox escalates too; active-and-watching stays healthy.
#    Unread mail age plays no part here — that alarm moved to mail-health.sh.
# 4. COMPOUND: a down delivery daemon plus aged concierge mail produces exactly one
#    liveness message (from watchd) and one aged-mail message (from mail-health.sh) — never
#    zero, never a duplicate of either.
# 5. LOG PATH: the unit's StandardOutput path is the exact path watchd's own status table
#    computes for this row — sp-12uu8's fixed defect, where the two literals had drifted apart
#    and nothing was ever appended to the path the watcher health row advertised as its log.
# 6. WAKE RETRY: a mailbox with unread mail and nobody reading gets woken again on
#    SPIRA_MAIL_WAKE_BACKOFF, not once and forgotten (T1); reading the mail is what stops
#    the retries, not a retry ceiling (T2). sp-12uu8: "confirm delivery and retry" alone is
#    exactly-once in spirit — dropped in favour of at-least-once-until-read.
# 7. LIVENESS SETTLE (UC-operator-channel-38): a `--kind event` message (a machine event —
#    PR/watcher transitions) is not held for SPIRA_MAIL_SETTLE, the window that batches the
#    OPERATOR's own replies landing in the same mailbox; it gets SPIRA_MAIL_SETTLE_EVENT
#    instead. Proven three ways: an event present from the start settles fast; a reply-only
#    burst still waits the full reply window; and an event arriving MID-WAY THROUGH an
#    already-running reply wait cuts it short rather than queuing behind it — the chosen
#    design flushes the pending reply early too, rather than tracking two timers.
#
# systemctl is mocked so no real unit manager is touched.
#
# scar: sp-m3zxv — mail delivery daemon died in a world halt and stayed dead for 26h;
# seven operator verdicts reached nobody.
# scar: sp-12uu8 — two operator replies produced no notification; the wake was a fire-and-
# forget tmux keystroke with no ack, and the log the watcher health row named was never
# written to.
# scar: a health probe that reads unread-mail age duplicated mail-health.sh's own alarm and
# could double-escalate one down-daemon-plus-backlog condition; narrowed to liveness only.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
SERVICE="$HERE/../systemd/spira-mail-deliver.service"
WORLD=world   # the world binary, by name on the suite PATH (sp-gypjk; world.sh retired by sp-6onps)
WATCHD=watchd    # sp-48f6g: watchd.sh rewritten to the compiled binary; invoked by name too

echo
echo "1. SERVICE UNIT — SuccessExitStatus=143:"

if [ -f "$SERVICE" ]; then
    ok "service file exists (positive control)"
else
    bad "service file exists (positive control)" "$SERVICE not found"
fi

if grep -q 'SuccessExitStatus=143' "$SERVICE"; then
    ok "SuccessExitStatus=143 is present"
else
    bad "SuccessExitStatus=143 is present" "SIGTERM exit 143 would leave unit 'failed'; world.sh start cannot revive a failed unit"
fi

# ---------------------------------------------------------------------------
echo
echo "2. WORLD.SH START — behavioural, against a mocked systemctl:"

WTMP="$(mktemp -d)"; trap 'rm -rf "$WTMP"' EXIT INT TERM
WBIN="$WTMP/bin"; mkdir -p "$WBIN"
WRUN="$WTMP/run"; mkdir -p "$WRUN"
MD_UNIT="spira-mail-deliver.service"
MOCK_LOG="$WTMP/systemctl-start.log"

cat > "$WBIN/systemctl" <<MOCK
#!/usr/bin/env bash
shift  # --user
cmd="\$1"; shift
case "\$cmd" in
    is-enabled)
        u="\$1"
        [ "\$u" = "$MD_UNIT" ] && echo "\${MD_ENABLED:-enabled}" || echo "enabled"
        ;;
    is-active)
        u="\$1"
        [ "\$u" = "$MD_UNIT" ] && echo "\${MD_ACTIVE:-inactive}" || echo "active"
        ;;
    list-unit-files)
        case "\$1" in
            *mail-deliver*) [ "\${MD_LISTED:-1}" = 1 ] && echo "$MD_UNIT" ;;
            *) : ;;
        esac
        ;;
    list-units) : ;;
    show)
        for a in "\$@"; do case "\$a" in *.service) printf 'Type=oneshot\n' ;; esac; done
        ;;
    start)
        for u in "\$@"; do echo "start \$u" >> "$MOCK_LOG"; done
        ;;
esac
exit 0
MOCK
chmod +x "$WBIN/systemctl"

tl_config SPIRA_RUN="$WRUN"
run_start() {
    : > "$MOCK_LOG"
    env -i HOME="$WTMP/home" PATH="$WBIN:$PATH" \
        SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" \
        SPIRA_SYSTEMCTL="$WBIN/systemctl" \
        SPIRA_TOML="$SPIRA_TOML" \
        "${@}" "$WORLD" start >/dev/null 2>&1
}

echo
echo "2a. enabled + inactive -> revived:"
run_start MD_ENABLED=enabled MD_ACTIVE=inactive
started="$(grep -c "^start $MD_UNIT\$" "$MOCK_LOG" 2>/dev/null || true)"
is "start revives an enabled-but-inactive mail-deliver unit" "1" "${started:-0}"

echo
echo "2b. disabled -> left alone:"
run_start MD_ENABLED=disabled MD_ACTIVE=inactive
started="$(grep -c "^start $MD_UNIT\$" "$MOCK_LOG" 2>/dev/null || true)"
is "start skips a disabled mail-deliver unit" "0" "${started:-0}"

echo
echo "2c. already active -> not restarted (idempotent):"
run_start MD_ENABLED=enabled MD_ACTIVE=active
started="$(grep -c "^start $MD_UNIT\$" "$MOCK_LOG" 2>/dev/null || true)"
is "start is idempotent for an already-active mail-deliver unit" "0" "${started:-0}"

# ---------------------------------------------------------------------------
echo
echo "3. LIVENESS — the extern health command is daemon liveness only:"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP" "$WTMP"' EXIT
mkdir -p "$TMP/home"
RUN="$TMP/run"; mkdir -p "$RUN"
WDIR="$RUN/watchd"; mkdir -p "$WDIR"
MOCK_BIN="$TMP/bin"; mkdir -p "$MOCK_BIN"
MAIL="$TMP/mail"
CONCIERGE_NEW="$MAIL/concierge/new"
mkdir -p "$CONCIERGE_NEW" "$MAIL/concierge/cur" "$MAIL/concierge/tmp"

# Manifest: mail-deliver as extern watcher. The health command runs the daemon's own
# script via SPIRA_HOME (set by conf.sh wherever it is sourced) — asking it whether it is
# watching its registered mailboxes, nothing about mail age.
MAN="$TMP/watchers"
printf 'mail-deliver|extern|mail-deliver|@SPIRA_HOME@/spira-mail-deliver.sh health\n' > "$MAN"

# Mock systemctl: is-enabled exits 0 (enabled); is-active/show answer with $ACTIVE_STATE
# (default inactive) so a test selects the unit state without a second mock.
cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
shift  # --user
cmd="$1"; shift
case "$cmd" in
    is-active)  for _u in "$@"; do echo "${ACTIVE_STATE:-inactive}"; done ;;
    show)
        for _a in "$@"; do
            case "$_a" in
                *.service) printf 'Id=%s\nActiveState=%s\nNRestarts=0\n\n' "$_a" "${ACTIVE_STATE:-inactive}" ;;
            esac
        done ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"

BASE_ENV=(
    HOME="$TMP/home"
    PATH="$MOCK_BIN:$PATH"
    SPIRA_CONF=/nonexistent
    SPIRA_MAIL_OPERATOR_CONSIDERED=test-suite
    SPIRA_HOME="$HERE"
    SPIRA_TOML="$SPIRA_TOML"
)
# SPIRA_MAIL_REPEAT_WINDOW=0: this section sends several DISTINCT escalations to operator
# with the SAME literal subject ("A watcher has stopped producing events") in one shared
# SPIRA_RUN — mail's own repeat-check would otherwise silently swallow every one after the
# first, which is correct anti-spam behaviour in production and exactly wrong for a test
# proving each condition escalates on its own.
# SPIRA_MAIL_MUTE=0: the complete fixture declares mail_mute = true, which delivers to
# "cur" (already-seen) rather than "new" (mail_deliver, sp-9hwim) — every asks()/grep check
# below reads "$MAIL/operator/new", so this suite declares its own unmuted intent (one
# source of config, per Ryan 2026-10-05), persisting for every later section too (sections
# 4-8 want the same unmuted behaviour).
# SPIRA_MAIL_KINDS="$HERE/mail/kinds": the complete fixture's default names a release-shaped
# path ("/fixture/userhome/.../spira-releases/current/spira/mail/kinds") that conf.sh no longer
# derives from SPIRA_HOME; watchd's escalation sends "--kind question", and `mail send`
# refuses any kind it cannot find a <kind>.md file for in SPIRA_MAIL_KINDS. Declare this
# suite's own real checkout, the same pattern test-mail-aeon.sh uses for SPIRA_CHAMBER.
tl_config SPIRA_RUN="$RUN" SPIRA_INSTANCE=test SPIRA_WATCHERS="$MAN" SPIRA_MAIL="$MAIL" \
    SPIRA_NOTIFY_AGE=0 SPIRA_ACTIONABLE=WAKEME SPIRA_MAIL_REPEAT_WINDOW=0 SPIRA_MAIL_MUTE=0 \
    SPIRA_MAIL_KINDS="$HERE/mail/kinds"

run_notify() {
    # Extra args split: SPIRA_* overrides (SPIRA_MAIL_READERS, SPIRA_MAIL_UNREAD_AGE) are
    # registered keys, declared via tl_config; ACTIVE_STATE is a mock-systemctl-only knob,
    # forwarded through env -i as before.
    local -a _passthrough=()
    local _kv
    for _kv in "$@"; do
        case "$_kv" in
            SPIRA_*) tl_config "$_kv" ;;
            *) _passthrough+=("$_kv") ;;
        esac
    done
    env -i "${BASE_ENV[@]}" \
        "${_passthrough[@]}" \
        watchd notify 2>/dev/null
    local _rc=$?
    # sp-pnogc (round 209, check 8): a real condition, not a timing guess — bash prints
    # "Terminated" and reports 128+signal when a foreground child dies by signal. Under a
    # full-corpus sweep this `watchd notify` (which, for 3c/3d, really execs the health
    # probe's own bash+pgrep) can be sent SIGTERM mid-run by testenv's own per-suite wall
    # budget before it ever reaches the escalation it was asked to prove. Every caller here
    # used to swallow that with `|| true`, indistinguishable from "ran fine, found nothing
    # to escalate" — the exact false red this check flipped on. A signal-killed run proves
    # nothing either way: bail, rather than let the next assertion misreport it.
    if [ "$_rc" -gt 128 ]; then
        bail "watchd notify was killed (signal $((_rc - 128))) mid-run — not a real assertion result"
    fi
    return "$_rc"
}

ROUTED='Routed here from an operator ask'
ask_files() { { find "$MAIL/operator/new" -type f 2>/dev/null; grep -rl "$ROUTED" "$MAIL/concierge/new" 2>/dev/null; } || true; }
asks()     { ask_files | wc -l | tr -d ' '; }
reset_run() {
    rm -rf "$MAIL/operator"
    grep -rl "$ROUTED" "$MAIL/concierge/new" 2>/dev/null | xargs -r rm -f
    rm -f "$WDIR/"*.unhealthy "$WDIR/notify-health.escalated" 2>/dev/null
}
# An old unread message in the concierge mailbox (mtime = 1 hour ago).
plant_mail() {
    local f="$CONCIERGE_NEW/verdict-1"
    : > "$f"
    touch -d "@$(( $(date +%s) - 3600 ))" "$f" 2>/dev/null || \
        touch -t "$(date -d '1 hour ago' '+%Y%m%d%H%M.%S' 2>/dev/null)" "$f" 2>/dev/null || true
}
backdate() { printf '%s\n' "$(( $(date +%s) - 3600 ))" > "$1"; }

# Unhealthy file for the mail-deliver watcher.
MD_UF="$WDIR/mail-deliver.unhealthy"

echo
echo "3a. POSITIVE CONTROL — daemon inactive, mailbox EMPTY -> escalation fires anyway:"
# No mail is planted here at all: proves the old aged-mail gate is gone from this path.
reset_run
backdate "$MD_UF"
run_notify ACTIVE_STATE=inactive || true
is "escalation fires on inactive daemon alone" "1" "$(asks)"

echo
echo "3b. DEDUP — second notify pass with the same condition -> no new escalation:"
run_notify ACTIVE_STATE=inactive || true
is "second notify does not file a duplicate escalation" "1" "$(asks)"

echo
echo "3c. active but NOT watching its registered mailbox -> escalation fires:"
reset_run
backdate "$MD_UF"
run_notify ACTIVE_STATE=active SPIRA_MAIL_READERS="concierge=echo wake" || true
is "escalation fires when active but not watching a registered mailbox" "1" "$(asks)"

echo
echo "3d. active AND watching its registered mailbox -> healthy, no escalation:"
reset_run
backdate "$MD_UF"
printf '%s\n' "$(( $(date +%s) + 60 ))" > "$RUN/mail-deliver-concierge.lease"
run_notify ACTIVE_STATE=active SPIRA_MAIL_READERS="concierge=echo wake" || true
rm -f "$RUN/mail-deliver-concierge.lease"
is "no escalation when watching every registered mailbox" "0" "$(asks)"
is "unhealthy file is cleared once healthy" "1" "$([ -f "$MD_UF" ] && echo 0 || echo 1)"

# ---------------------------------------------------------------------------
echo
echo "4. COMPOUND — a down delivery daemon plus aged concierge mail:"

reset_run
plant_mail
backdate "$MD_UF"
run_notify ACTIVE_STATE=inactive SPIRA_MAIL_READERS="concierge=echo wake" SPIRA_MAIL_UNREAD_AGE=0 || true

total="$(asks)"
liveness="$(ask_files | xargs -r grep -l 'stopped producing events' 2>/dev/null | wc -l | tr -d ' ')"
aged="$(ask_files | xargs -r grep -l 'is not reading its mail' 2>/dev/null | wc -l | tr -d ' ')"

is "4a: exactly two operator messages total" "2" "$total"
is "4b: exactly one liveness message from watchd" "1" "$liveness"
is "4c: exactly one aged-mail message from mail-health.sh" "1" "$aged"

# ---------------------------------------------------------------------------
echo
echo "5. LOG PATH — the unit's StandardOutput is the path watchd's own status table computes:"

# Ask the real watchd (sp-48f6g: watchd.sh rewritten to the compiled binary; _wd_logfile no
# longer exists to source) what path an extern row named "mail-deliver" logs to, via its
# `status` table's own LOG column — rather than re-typing the "watchd/<name>.log" convention
# as a second literal here.
LOGRUN="$TMP/logpath-run"
LOGWATCHERS="$TMP/logpath-watchers"
printf 'mail-deliver|extern|mail-deliver\n' > "$LOGWATCHERS"
tl_config SPIRA_RUN="$LOGRUN" SPIRA_WATCHERS="$LOGWATCHERS"
expected_logfile="$(
    env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" \
        SPIRA_TOML="$SPIRA_TOML" \
        "$WATCHD" status 2>/dev/null \
        | awk '$1=="mail-deliver" { print $NF }'
)"
is "5a: watchd status computes the log under watchd/" "$LOGRUN/watchd/mail-deliver.log" "$expected_logfile"

unit_line="$(grep -m1 '^StandardOutput=append:' "$SERVICE")"
unit_resolved="${unit_line#StandardOutput=append:}"
unit_resolved="${unit_resolved//@SPIRA_RUN@/$LOGRUN}"
is "5b: unit's StandardOutput is the path _wd_logfile computes" "$expected_logfile" "$unit_resolved"

# ---------------------------------------------------------------------------
# _wake_count <file> -> lines containing "wake sent". Not `grep -c ... || echo 0`: grep -c
# already prints 0 on no match, but also exits 1 on no match, so the fallback fires anyway
# and doubles the output.
_wake_count() { grep -c 'wake sent' "$1" 2>/dev/null; true; }

# _bg_exited <pid> <max 0.1s ticks> -> 0 once the pid is gone, 1 if it outlives the budget.
_bg_exited() {
    local pid="$1" n="${2:-50}"
    while kill -0 "$pid" 2>/dev/null; do
        n=$((n-1))
        [ "$n" -le 0 ] && return 1
        sleep 0.1
    done
    return 0
}

echo
echo "6. WAKE RETRY — T1: nobody reads; the wake is not fired once and forgotten:"

WMAIL1="$TMP/wake-mail-1"; mkdir -p "$WMAIL1/wakebox/new" "$WMAIL1/wakebox/cur" "$WMAIL1/wakebox/tmp"
WRUN1="$TMP/wake-run-1"; mkdir -p "$WRUN1"
: > "$WMAIL1/wakebox/new/msg1"

CALLS1="$TMP/wake-calls-t1.log"; : > "$CALLS1"
cat > "$TMP/wake-stub-t1.sh" <<STUB
#!/usr/bin/env bash
printf '%s\t%s\n' "\$(date +%s)" "\$1" >> "$CALLS1"
STUB
chmod +x "$TMP/wake-stub-t1.sh"

ATT1="$TMP/wake-attempts-t1.log"; : > "$ATT1"
tl_config SPIRA_RUN="$WRUN1" SPIRA_MAIL="$WMAIL1" SPIRA_MAIL_WAKE_BACKOFF="1 1 1 1"
env -i HOME="$TMP/home" PATH="$PATH" \
    SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
    bash -c '. "$1"; _wake_loop wakebox "$2"' _ "$HERE/spira-mail-deliver.sh" "$TMP/wake-stub-t1.sh" \
    > "$ATT1" 2>&1 &
LOOP1_PID=$!

start=$SECONDS
tries=0
while [ "$(_wake_count "$ATT1")" -lt 3 ] && [ "$tries" -lt 150 ]; do
    sleep 0.1
    tries=$((tries+1))
done
elapsed=$(( SECONDS - start ))
kill "$LOOP1_PID" 2>/dev/null
_bg_exited "$LOOP1_PID" 20

attempts1="$(_wake_count "$ATT1")"
calls1="$(wc -l < "$CALLS1" 2>/dev/null | tr -d ' ')"
last_msg="$(tail -1 "$CALLS1" | cut -f2-)"

is "6a: wake fires more than once while unread and unread" "1" "$([ "$attempts1" -ge 3 ] && echo 1 || echo 0)"
is "6b: the stub wake command was actually invoked each time" "$attempts1" "${calls1:-0}"
is "6c: backoff delays retries (3 attempts at 1s steps takes >=2s, not 0)" "1" "$([ "$elapsed" -ge 2 ] && echo 1 || echo 0)"
want "6d: the wake message says how many are unread" "You have 1 unread messages" "$last_msg"
want "6e: the wake message says how old the oldest unread is" "oldest" "$last_msg"

echo
echo "7. WAKE RETRY — T2: reading the mail stops it (reading is the ack, not the wake):"

WMAIL2="$TMP/wake-mail-2"; mkdir -p "$WMAIL2/wakebox/new" "$WMAIL2/wakebox/cur" "$WMAIL2/wakebox/tmp"
WRUN2="$TMP/wake-run-2"; mkdir -p "$WRUN2"
: > "$WMAIL2/wakebox/new/msg1"

CALLS2="$TMP/wake-calls-t2.log"; : > "$CALLS2"
cat > "$TMP/wake-stub-t2.sh" <<STUB
#!/usr/bin/env bash
printf '%s\t%s\n' "\$(date +%s)" "\$1" >> "$CALLS2"
STUB
chmod +x "$TMP/wake-stub-t2.sh"

ATT2="$TMP/wake-attempts-t2.log"; : > "$ATT2"
tl_config SPIRA_RUN="$WRUN2" SPIRA_MAIL="$WMAIL2" SPIRA_MAIL_WAKE_BACKOFF="1 1 1 1"
env -i HOME="$TMP/home" PATH="$PATH" \
    SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
    bash -c '. "$1"; _wake_loop wakebox "$2"' _ "$HERE/spira-mail-deliver.sh" "$TMP/wake-stub-t2.sh" \
    > "$ATT2" 2>&1 &
LOOP2_PID=$!

tries=0
while [ "$(_wake_count "$ATT2")" -lt 1 ] && [ "$tries" -lt 150 ]; do
    sleep 0.1
    tries=$((tries+1))
done
first_seen="$(_wake_count "$ATT2")"

env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" SPIRA_TOML="$SPIRA_TOML" \
    mail read wakebox >/dev/null 2>&1

sleep 2.5
loop_exited=0; _bg_exited "$LOOP2_PID" 30 && loop_exited=1
after_read="$(_wake_count "$ATT2")"

is "7a: at least one wake fired before the mail was read" "1" "$([ "$first_seen" -ge 1 ] && echo 1 || echo 0)"
is "7b: the wake loop exits once the mail is read, no retry ceiling needed" "1" "$loop_exited"
is "7c: no further wake after the mail was read" "$first_seen" "$after_read"

# ---------------------------------------------------------------------------
echo
echo "8. LIVENESS SETTLE — a machine event does not sit through the reply-batching window:"

SDIR="$TMP/settle-dir"

msg_reply() { printf 'From: Ryan <ryan@spira>\nSubject: ok\n\nbody\n'; }
msg_event() { printf 'From: PR Notify <pr-notify@spira>\nSubject: pr-notify: FAIL RED\nX-Spira-Kind: event\n\nFAIL RED #1 x [queue-repo]: suites\n'; }

echo
echo "8a. POSITIVE CONTROL — an event present from the start settles near-instantly:"
rm -rf "$SDIR"; mkdir -p "$SDIR"
msg_event > "$SDIR/event1"
t0=$SECONDS
tl_config SPIRA_MAIL_SETTLE=10 SPIRA_MAIL_SETTLE_EVENT=0
env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
    bash -c '. "$1"; _settle_wait "$2"' _ "$HERE/spira-mail-deliver.sh" "$SDIR"
elapsed=$((SECONDS - t0))
is "8a: an already-present event settles well under the reply window" "1" "$([ "$elapsed" -le 2 ] && echo 1 || echo 0)"

echo
echo "8b. a reply-only burst still waits the full reply window (no regression):"
rm -rf "$SDIR"; mkdir -p "$SDIR"
msg_reply > "$SDIR/reply1"
t0=$SECONDS
tl_config SPIRA_MAIL_SETTLE=3 SPIRA_MAIL_SETTLE_EVENT=0
env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
    bash -c '. "$1"; _settle_wait "$2"' _ "$HERE/spira-mail-deliver.sh" "$SDIR"
elapsed=$((SECONDS - t0))
is "8b: reply-only settle takes the full SPIRA_MAIL_SETTLE window" "1" "$([ "$elapsed" -ge 3 ] && echo 1 || echo 0)"

echo
echo "8c. THE ACCEPTANCE CASE — an event arriving mid-way through an already-running reply"
echo "    wait is not delayed behind it (a reply pending in its window must not delay an event):"
rm -rf "$SDIR"; mkdir -p "$SDIR"
msg_reply > "$SDIR/reply1"
( sleep 2; msg_event > "$SDIR/event1" ) &
DROP_PID=$!
t0=$SECONDS
tl_config SPIRA_MAIL_SETTLE=10 SPIRA_MAIL_SETTLE_EVENT=0
env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
    bash -c '. "$1"; _settle_wait "$2"' _ "$HERE/spira-mail-deliver.sh" "$SDIR"
elapsed=$((SECONDS - t0))
wait "$DROP_PID" 2>/dev/null
is "8c: a mid-window event cuts the wait short, not the full reply window" "1" \
    "$([ "$elapsed" -ge 2 ] && [ "$elapsed" -le 4 ] && echo 1 || echo 0)"
is "8c: well under the 10s liveness bound end to end" "1" "$([ "$elapsed" -le 10 ] && echo 1 || echo 0)"

echo
echo "9. WATCH LEASE — health is the lease, never a process scan:"
LRUN="$TMP/lease-run"; mkdir -p "$LRUN"
tl_config SPIRA_RUN="$LRUN" SPIRA_MAIL_READERS="leasebox=echo wake"
lease_health() {
    env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
        bash "$HERE/spira-mail-deliver.sh" health >/dev/null 2>&1
    echo $?
}
is "9a: no lease -> not watching" "1" "$(lease_health)"
env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
    bash -c '. "$1"; _renew_watch_lease leasebox' _ "$HERE/spira-mail-deliver.sh"
is "9b: a renewed lease -> watching" "0" "$(lease_health)"
printf '%s\n' "$(( $(date +%s) - 5 ))" > "$LRUN/mail-deliver-leasebox.lease"
is "9c: an expired lease -> not watching" "1" "$(lease_health)"

tl_summary
