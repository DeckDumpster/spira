#!/usr/bin/env bash
# tier: T2
# covers: systemd/spira-mail-deliver.service spira/world.sh spira/watchd.sh spira/watchers UC-operator-channel-10
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
#
# systemctl is mocked so no real unit manager is touched.
#
# scar: sp-m3zxv — mail delivery daemon died in a world halt and stayed dead for 26h;
# seven operator verdicts reached nobody.
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

tl_summary
