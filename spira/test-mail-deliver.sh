#!/usr/bin/env bash
# tier: T2
# covers: systemd/spira-mail-deliver.service spira/world.sh spira/watchd.sh spira/watchers spira/spira-mail-deliver.sh spira/mail.sh spira/conf.sh UC-operator-channel-10
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. SERVICE UNIT: SuccessExitStatus=143 is set — a halt's SIGTERM exit is clean (inactive,
#    not failed), so world.sh start can revive the unit.
# 2. WORLD.SH START: behavioural — against a mocked systemctl, `start` revives
#    spira-mail-deliver when it is enabled-but-inactive, leaves a disabled unit alone, and
#    does not restart an already-active one (coverage-map row 10, SOURCE-GREP: a source
#    grep proved the code MENTIONS these rules; running `start` proves it OBEYS them).
# 3. DETECTOR: watchd notify files exactly one escalation when the delivery daemon is
#    inactive AND there are unread concierge replies older than SPIRA_NOTIFY_AGE.  With
#    no unread mail the compound condition is not met and no escalation fires.
# 4. LOG PATH: the unit's StandardOutput path is the exact path watchd.sh's own _wd_logfile
#    computes for this row — sp-12uu8's fixed defect, where the two literals had drifted apart
#    and nothing was ever appended to the path the watcher health row advertised as its log.
# 5. WAKE RETRY: a mailbox with unread mail and nobody reading gets woken again on
#    SPIRA_MAIL_WAKE_BACKOFF, not once and forgotten (T1); reading the mail is what stops
#    the retries, not a retry ceiling (T2). sp-12uu8: "confirm delivery and retry" alone is
#    exactly-once in spirit — dropped in favour of at-least-once-until-read.
#
# systemctl is mocked so no real unit manager is touched.
#
# scar: sp-m3zxv — mail delivery daemon died in a world halt and stayed dead for 26h;
# seven operator verdicts reached nobody.
# scar: sp-12uu8 — two operator replies produced no notification; the wake was a fire-and-
# forget tmux keystroke with no ack, and the log the watcher health row named was never
# written to.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
SERVICE="$HERE/../systemd/spira-mail-deliver.service"
WORLD="$HERE/world.sh"
WATCHD="$HERE/watchd.sh"

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

run_start() {
    : > "$MOCK_LOG"
    env -i HOME="$WTMP/home" PATH="$WBIN:$PATH" \
        SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" SPIRA_RUN="$WRUN" \
        SPIRA_SYSTEMCTL="$WBIN/systemctl" \
        "${@}" bash "$WORLD" start >/dev/null 2>&1
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
echo "3. DETECTOR — compound condition: daemon inactive AND unread concierge mail:"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP" "$WTMP"' EXIT
mkdir -p "$TMP/home"
RUN="$TMP/run"; mkdir -p "$RUN"
WDIR="$RUN/watchd"; mkdir -p "$WDIR"
MOCK_BIN="$TMP/bin"; mkdir -p "$MOCK_BIN"
MAIL="$TMP/mail"
CONCIERGE_NEW="$MAIL/concierge/new"; mkdir -p "$CONCIERGE_NEW"

# Manifest: mail-deliver as extern watcher. The health command calls mail.sh using
# SPIRA_HOME (set by conf.sh to the harness directory when watchd.sh sources it).
MAN="$TMP/watchers"
printf 'mail-deliver|extern|mail-deliver|@SPIRA_HOME@/mail.sh unread-age concierge\n' > "$MAN"

# Mock systemctl: is-enabled exits 0 (enabled), is-active echoes inactive,
# show returns ActiveState=inactive for any *.service.
cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
shift  # --user
cmd="$1"; shift
case "$cmd" in
    is-active)  for _u in "$@"; do echo "inactive"; done ;;
    show)
        for _a in "$@"; do
            case "$_a" in
                *.service) printf 'Id=%s\nActiveState=inactive\nNRestarts=0\n\n' "$_a" ;;
            esac
        done ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"

BASE_ENV=(
    HOME="$TMP/home"
    PATH="$PATH"
    SPIRA_CONF=/nonexistent
    SPIRA_PATH="$MOCK_BIN"
    SPIRA_RUN="$RUN"
    SPIRA_INSTANCE=test
    SPIRA_WATCHERS="$MAN"
    SPIRA_MAIL="$MAIL"
)

run_notify() {
    env -i "${BASE_ENV[@]}" \
        SPIRA_NOTIFY_AGE=0 \
        SPIRA_ACTIONABLE=WAKEME \
        bash "$WATCHD" notify 2>/dev/null
}

asks()     { find "$MAIL/operator/new" -type f 2>/dev/null | wc -l | tr -d ' '; }
reset_run() {
    rm -rf "$MAIL/operator"
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
echo "3a. POSITIVE CONTROL — daemon inactive + old unread mail -> escalation fires:"
reset_run
plant_mail
backdate "$MD_UF"
run_notify || true
is "escalation fires with inactive daemon and unread mail" "1" "$(asks)"

echo
echo "3b. DEDUP — second notify pass with same conditions -> no new escalation:"
run_notify || true
is "second notify does not file a duplicate escalation" "1" "$(asks)"

echo
echo "3c. COMPOUND CONDITION — daemon inactive but empty mailbox -> no escalation:"
reset_run
rm -f "$CONCIERGE_NEW"/* 2>/dev/null
backdate "$MD_UF"
run_notify || true
is "no escalation when mailbox is empty" "0" "$(asks)"
# Prove the path was reachable: 3a already fired with the same setup minus the mail.

# ---------------------------------------------------------------------------
echo
echo "4. LOG PATH — the unit's StandardOutput is the path watchd.sh's own _wd_logfile computes:"

# Ask watchd.sh's own function what path an extern row named "mail-deliver" logs to,
# rather than re-typing the "watchd/<name>.log" convention as a second literal here.
LOGRUN="$TMP/logpath-run"
expected_logfile="$(
    env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_RUN="$LOGRUN" \
        bash -c '. "$1"; _wd_logfile mail-deliver extern mail-deliver' _ "$WATCHD"
)"
is "4a: _wd_logfile computes the log under watchd/" "$LOGRUN/watchd/mail-deliver.log" "$expected_logfile"

unit_line="$(grep -m1 '^StandardOutput=append:' "$SERVICE")"
unit_resolved="${unit_line#StandardOutput=append:}"
unit_resolved="${unit_resolved//@SPIRA_RUN@/$LOGRUN}"
is "4b: unit's StandardOutput is the path _wd_logfile computes" "$expected_logfile" "$unit_resolved"

# ---------------------------------------------------------------------------
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
echo "5. WAKE RETRY — T1: nobody reads; the wake is not fired once and forgotten:"

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
env -i HOME="$TMP/home" PATH="$PATH" SPIRA_RUN="$WRUN1" SPIRA_MAIL="$WMAIL1" \
    SPIRA_CONF=/nonexistent SPIRA_MAIL_WAKE_BACKOFF="1 1 1 1" \
    bash -c '. "$1"; _wake_loop wakebox "$2"' _ "$HERE/spira-mail-deliver.sh" "$TMP/wake-stub-t1.sh" \
    > "$ATT1" 2>&1 &
LOOP1_PID=$!

start=$SECONDS
tries=0
while [ "$(grep -c 'wake sent' "$ATT1" 2>/dev/null || echo 0)" -lt 3 ] && [ "$tries" -lt 150 ]; do
    sleep 0.1
    tries=$((tries+1))
done
elapsed=$(( SECONDS - start ))
kill "$LOOP1_PID" 2>/dev/null
_bg_exited "$LOOP1_PID" 20

attempts1="$(grep -c 'wake sent' "$ATT1" 2>/dev/null || echo 0)"
calls1="$(wc -l < "$CALLS1" 2>/dev/null | tr -d ' ')"
last_msg="$(tail -1 "$CALLS1" | cut -f2-)"

is "5a: wake fires more than once while unread and unread" "1" "$([ "$attempts1" -ge 3 ] && echo 1 || echo 0)"
is "5b: the stub wake command was actually invoked each time" "$attempts1" "${calls1:-0}"
is "5c: backoff delays retries (3 attempts at 1s steps takes >=2s, not 0)" "1" "$([ "$elapsed" -ge 2 ] && echo 1 || echo 0)"
want "5d: the wake message says how many are unread" "You have 1 unread messages" "$last_msg"
want "5e: the wake message says how old the oldest unread is" "oldest" "$last_msg"

echo
echo "6. WAKE RETRY — T2: reading the mail stops it (reading is the ack, not the wake):"

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
env -i HOME="$TMP/home" PATH="$PATH" SPIRA_RUN="$WRUN2" SPIRA_MAIL="$WMAIL2" \
    SPIRA_CONF=/nonexistent SPIRA_MAIL_WAKE_BACKOFF="1 1 1 1" \
    bash -c '. "$1"; _wake_loop wakebox "$2"' _ "$HERE/spira-mail-deliver.sh" "$TMP/wake-stub-t2.sh" \
    > "$ATT2" 2>&1 &
LOOP2_PID=$!

tries=0
while [ "$(grep -c 'wake sent' "$ATT2" 2>/dev/null || echo 0)" -lt 1 ] && [ "$tries" -lt 150 ]; do
    sleep 0.1
    tries=$((tries+1))
done
first_seen="$(grep -c 'wake sent' "$ATT2" 2>/dev/null || echo 0)"

env -i HOME="$TMP/home" PATH="$PATH" SPIRA_MAIL="$WMAIL2" SPIRA_CONF=/nonexistent \
    bash "$HERE/mail.sh" read wakebox >/dev/null 2>&1

sleep 2.5
loop_exited=0; _bg_exited "$LOOP2_PID" 30 && loop_exited=1
after_read="$(grep -c 'wake sent' "$ATT2" 2>/dev/null || echo 0)"

is "6a: at least one wake fired before the mail was read" "1" "$([ "$first_seen" -ge 1 ] && echo 1 || echo 0)"
is "6b: the wake loop exits once the mail is read, no retry ceiling needed" "1" "$loop_exited"
is "6c: no further wake after the mail was read" "$first_seen" "$after_read"

tl_summary
