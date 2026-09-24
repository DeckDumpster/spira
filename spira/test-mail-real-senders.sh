#!/usr/bin/env bash
# test-mail-real-senders.sh — real harness emitters pass mail.sh's own lint (gap G-05,
# UC-operator-channel-05).
#
# test-migrate-ask.sh used to lint hand-copied sender bodies, so a regression in a real
# emitter's actual message construction went unseen (it tested copies, not the senders).
# This calls each emitter's real code — not a re-typed body — with mail.sh pointed at a
# scratch Maildir and with no lint override, so the running message actually clears
# mail.sh's send path.
#
# COVERAGE: land_escalate (lib.sh), watchd.sh's _wd_ask, and skew.sh's escalate are
# standalone functions reachable without standing up a database or systemd — sourced
# directly (skew.sh's escalate is extracted with sed rather than sourcing the whole file,
# because skew.sh has no main guard and runs a real box audit at source time otherwise).
#
# DEFERRED: incident.sh's SIN escalation and archivist.sh's session-drift notice are
# reachable only through their full subsystems (a real bead store counting recurrences;
# a live session transcript measured by ctx-meter.sh) — standing those up is follow-up
# work, not a lint-path check. See this suite's owning bead's close notes.
#
# tier: T2
# covers: spira/lib.sh spira/watchd.sh spira/skew.sh spira/mail.sh UC-operator-channel-05
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=/nonexistent
export SPIRA_ID_PREFIX="sp"
export SPIRA_ASK_LABEL="needs-operator"

# A stub bd: every emitter below only needs "is there already an open ask with this
# subject" to answer no, so a send is always attempted for real.
STUB_BD="$TMP/bd-stub.sh"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
printf '[]\n'
STUB
chmod +x "$STUB_BD"
export SPIRA_BD="$STUB_BD"
export SPIRA_DB="$TMP/db"

unread() { bash "$HERE/mail.sh" count operator 2>/dev/null; }

echo
echo "lib.sh: land_escalate (question)"

. "$HERE/lib.sh"
export SPIRA_LAND_ESCALATE_EVERY=0
before="$(unread)"
land_escalate "gate keeps failing in the real emitter test" \
    "every finished branch has been rejected by the landing gate." >/dev/null 2>&1
after="$(unread)"
is "land_escalate delivers exactly one message" "$((before + 1))" "$after"
msg="$(ls -t "$SPIRA_MAIL/operator/new" 2>/dev/null | head -1)"
body="$(cat "$SPIRA_MAIL/operator/new/$msg" 2>/dev/null)"
want "land_escalate message names the reason"  "gate keeps failing"  "$body"
want "land_escalate message carries a Default"  "## Default"          "$body"

echo
echo "watchd.sh: _wd_ask (question)"

. "$HERE/watchd.sh"
before="$(unread)"
_wd_ask "test-real-sender.escalated" "a fingerprint for the real-sender test" \
    "Events a watcher produced have reached no reader" \
    "read them below and act on them here" \
    "delivery otherwise depends on a session existing to drain them" \
    "evidence: the real-sender test planted this condition" >/dev/null 2>&1
after="$(unread)"
is "_wd_ask delivers exactly one message" "$((before + 1))" "$after"

echo
echo "skew.sh: escalate (question)"

ESCALATE_SRC="$TMP/skew-escalate.sh"
sed -n '/^escalate() {/,/^}/p' "$HERE/skew.sh" > "$ESCALATE_SRC"
[ -s "$ESCALATE_SRC" ] || bad "extracted skew.sh escalate() is non-empty" "sed found nothing — skew.sh's shape changed"
. "$ESCALATE_SRC"
before="$(unread)"
escalate "v2:REAL-SENDER-TEST=1" "planted by the real-sender test" >/dev/null 2>&1
after="$(unread)"
is "skew.sh escalate delivers exactly one message" "$((before + 1))" "$after"

tl_summary
