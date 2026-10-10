#!/usr/bin/env bash
#
# test-world-deadline-notice.sh — world drain mails each live aeon its deadline.
#
#   1. No live aeons: nothing is sent.
#   2. A live aeon with an open mailbox receives the notice, naming both choices.
#
# tier: T1
# covers: spira-world/src/notice.rs spira-world/src/bin/world.rs
set -uo pipefail
set -m
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-world-deadline-notice.sh"

TMP="$(mktemp -d)"
WORKER_PID=""
trap 'kill -- -"$WORKER_PID" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM

SH="$TMP/spira"
RUN="$TMP/run"
SLAY_CALLS="$TMP/slay-calls"
export SLAY_CALLS
mkdir -p "$SH" "$RUN" "$RUN/mail"

WORLD_BIN="$(command -v world || true)"
[ -n "$WORLD_BIN" ] && [ -x "$WORLD_BIN" ] || { echo "test-world-deadline-notice.sh: the world binary is not on PATH" >&2; exit 1; }
cp "$WORLD_BIN" "$SH/world.sh"; chmod +x "$SH/world.sh"
cp "$HERE/conf.sh" "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"

# Stub systemctl: answers inactive for everything so halt/drain banner checks stay quiet.
printf '#!/usr/bin/env bash\necho inactive\nexit 3\n' > "$TMP/systemctl"
chmod +x "$TMP/systemctl"

# Recording slay.sh: appends full argument list to SLAY_CALLS on each call, exits 0.
cat > "$SH/slay" <<'SLAY'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SLAY_CALLS"
exit 0
SLAY
chmod +x "$SH/slay"

# Fake aeon.sh so live_aeons() finds the process in /proc via argv match.
printf '#!/usr/bin/env bash\nsleep 120\n' > "$SH/aeon.sh"; chmod +x "$SH/aeon.sh"

# SPIRA_PROD/SPIRA_RUN/SPIRA_DB are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
# CONFIG): declare via tl_config, not the env prefixes below, which no process reads them
# from any more. Same values throughout this file, so one declaration covers every call.
tl_config SPIRA_PROD="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$TMP/no-db"

drain() {
    rc=0
    out="$(PATH="$SH:$PATH" SPIRA_HOME="$SH" SPIRA_CONF="$TMP/no.conf" \
           SPIRA_SYSTEMCTL="$TMP/systemctl" \
           "$SH/world.sh" drain "$@" 2>&1)" || rc=$?
}

TEST_BEAD="sp-drain-test"

# --------------------------------------------------------------------------------------
# 1. POSITIVE CONTROL — no live aeons.
# --------------------------------------------------------------------------------------
MAILBOX="$RUN/mail/aeon-sp-notice-test"
count() { find "$MAILBOX/new" -type f 2>/dev/null | wc -l | tr -d ' '; }
mkdir -p "$MAILBOX/new" "$MAILBOX/cur" "$MAILBOX/tmp"
tl_config SPIRA_MAIL="$RUN/mail" SPIRA_MAIL_MUTE=0

echo
echo "no live aeons — nothing sent:"
drain --deadline 0
is "no notice without a live aeon" 0 "$(count)"

echo
echo "a live aeon is mailed its deadline:"
bash "$SH/aeon.sh" & WORKER_PID=$!
sleep 0.3
printf '%s\n' "$WORKER_PID" > "$RUN/aeon-valefor-sp-notice-test.pid"
printf '%s' "$(( $(date +%s) + 3600 ))" > "$RUN/aeon-valefor-sp-notice-test.lease"
rm -f "$RUN/world.draining"
drain --deadline 0
is "exactly one notice delivered" 1 "$(count)"
msg="$(cat "$MAILBOX"/new/* 2>/dev/null)"
want "names the drain" "draining" "$msg"
want "offers submit" "work submit" "$msg"
want "offers a checkpoint" "WIP checkpoint" "$msg"
want "asks for a bead note" "work note" "$msg"

rm -f "$RUN/aeon-valefor-sp-notice-test.pid" "$RUN/aeon-valefor-sp-notice-test.lease"
kill -- -"$WORKER_PID" 2>/dev/null; wait "$WORKER_PID" 2>/dev/null; WORKER_PID=""
tl_summary
