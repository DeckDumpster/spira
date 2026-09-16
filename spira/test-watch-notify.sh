#!/usr/bin/env bash
#
# test-watch-notify.sh — delivery that does not need a reader, and the once-only property
# that is the whole reason it is allowed to exist.
#
#   ./test-watch-notify.sh
#
# WHAT IT HOLDS. `watchd.sh notify` is a timer that escalates events nobody has drained, so
# an answer given while no session was running still reaches somebody. Every failure it can
# have is a way of being worse than nothing:
#
#   1. ONE ESCALATION PER BACKLOG, NEVER ONE PER PASS. The condition persists until somebody
#      acts on it, so a timer that asked again every five minutes would be a false-alarm
#      generator — and a false alarm is a real cost, because the noise is what teaches an
#      operator to scroll past the one that matters (law-alerts-must-be-actionable).
#   2. DRAINING CLEARS IT, AND THE NEXT BACKLOG IS HEARD. Suppression that outlived its
#      condition would swallow the second occurrence entirely, which is the same silence
#      arriving later.
#   3. THE CURSOR IS NOT TOUCHED. Escalating is an extra copy of the event, never a
#      substitute for it: a notify that marked what it reported as read would make the ask
#      the only delivery and would clear the condition it was reporting on.
#   4. IT REFUSES RATHER THAN PASSING when it could not check — a malformed manifest, an
#      unreadable threshold, no filter — because "nothing is waiting" and "I could not look"
#      are the same exit code otherwise (law-absence-needs-a-positive-control).
#   5. A FINDING IS NOT CONSUMED BY A BROKEN CHANNEL. If the escalation path refuses, nothing
#      is stamped, so the retry once it is repaired still carries the event.
#
# It needs no database, no beads server and no systemd: every watcher here is a `log` row,
# which is a file something else writes, and the escalation path is a stub that records what
# it was handed.
#
# In an explicit, minimal environment, with SPIRA_CONF pointed at a file that does not exist
# so the operator's own configuration cannot decide a verdict. SPIRA_ACTIONABLE is pinned to
# a word that appears in no shipped default, and the decoy lines below are written in the
# SHIPPED vocabulary — so a filter expression written into the code instead of read from
# configuration fails here rather than passing by coincidence.
#
# defect: sp-ee4
# covers: spira/watchd.sh spira/watchers systemd/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"

pass=0; fail=0
ok()  { printf '  ok    %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n        got: %s\n' "$1" "$2"; fail=$((fail+1)); }
is()  { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "want [$2] got [$3]"; fi; }
has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "$2" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1" "$2" ;; *) ok "$1" ;; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home"

# A harness tree that is NOT this checkout, so nothing here can read the operator's own
# configuration and report a pass it did not earn.
CLONE="$TMP/clone"
mkdir -p "$CLONE/spira" "$CLONE/cockpit"
cp "$HERE/conf.sh" "$HERE/watchd.sh" "$HERE/mail-health.sh" "$CLONE/spira/"
# NOMAIL: a clone without mail.sh — used for the "no delivery path" test case.
NOMAIL="$TMP/nomail"
mkdir -p "$NOMAIL"
cp "$HERE/conf.sh" "$HERE/watchd.sh" "$NOMAIL/"
cp -r "$ROOT/systemd" "$CLONE/systemd"

# EVERY CONFIGURED VALUE IS PINNED TO A NON-DEFAULT. SPIRA_RUN would derive to
# $CLONE/.runtime/spira and SPIRA_COCKPIT to $CLONE/cockpit; both are moved somewhere
# unrelated, so a literal written into the code cannot pass.
RUN="$TMP/elsewhere/run"; COCKPIT="$TMP/elsewhere/cockpit"
mkdir -p "$RUN" "$COCKPIT"
CONF="$TMP/spira.conf"
printf 'SPIRA_RUN = %s\nSPIRA_COCKPIT = %s\n' "$RUN" "$COCKPIT" > "$CONF"

# The escalation seam, stubbed to a log. What is asserted here is that a mail is SENT, once,
# and that it carries the event as its evidence — never what the mailbox does with it after.
ASKS="$TMP/asks.log"; : > "$ASKS"
cat > "$CLONE/spira/mail.sh" <<'MAILSH'
#!/usr/bin/env bash
[ "${1:-}" = send ] || exit 0
[ -n "${NOTIFY_REFUSE:-}" ] && exit 1
{ printf '=== ask\n'; printf '%s\n' "$@"; printf '\n'; cat; printf '\n'; } >> "$NOTIFY_LOG"
MAILSH
chmod +x "$CLONE/spira/mail.sh"

# The two watchers. Both are `log` rows: something else writes the file, which is exactly what
# this suite wants — a watcher whose events it can author line by line, with no unit to start.
A="$TMP/a.log"; B="$TMP/b.log"
MAN="$TMP/watchers"
{ printf 'alpha|log|%s\n' "$A"; printf 'beta|log|%s\n' "$B"; } > "$MAN"

# FILTER PINNED TO A NON-DEFAULT WORD. Nothing in the shipped SPIRA_ACTIONABLE matches it.
FILTER="WAKEME"

# notify <age> [manifest] -> rc; stdout in $TMP/out, stderr in $TMP/err
notify() {
    env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$CONF" \
        SPIRA_WATCHERS="${2:-$MAN}" SPIRA_ACTIONABLE="${FILTER_OVERRIDE-$FILTER}" \
        SPIRA_NOTIFY_AGE="$1" \
        NOTIFY_LOG="$ASKS" ${NOTIFY_REFUSE:+NOTIFY_REFUSE=1} \
        bash "$CLONE/spira/watchd.sh" notify > "$TMP/out" 2> "$TMP/err"
}
# wd <args...> — any other watchd command, in the same environment.
wd() {
    env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$CONF" \
        SPIRA_WATCHERS="$MAN" SPIRA_ACTIONABLE="$FILTER" \
        bash "$CLONE/spira/watchd.sh" "$@"
}
# `grep -c` on a file with no match exits 1, so the count must be captured rather than
# chained — `|| echo 0` appends a second line and every comparison against it fails.
asks() { local n; n="$(grep -c '=== ask' "$ASKS" 2>/dev/null)" || n=0; printf '%s' "$n"; }
reset() { : > "$A"; : > "$B"; rm -rf "$RUN/watchd"; : > "$ASKS"; }
# mature_pending — backdate every pending file by 3600 s so any standing event appears
# older than any threshold used in this suite. Without this, each maturation test must
# sleep 1 s (threshold=1) per event, which accumulates to more than the 25 s suite timeout.
# The format is "<line-number> <epoch>", written and read only by cmd_notify in watchd.sh.
mature_pending() {
    local d="$RUN/watchd" f apos at
    for f in "$d/"*.pending; do
        [ -f "$f" ] || continue
        read -r apos at < "$f"
        printf '%s %s\n' "$apos" "$(( at - 3600 ))" > "$f"
    done
}

echo "test-watch-notify.sh"

# =======================================================================================
# The positive control. Every "no ask was raised" assertion below is worthless unless this
# pass proves the command can run at all against this manifest and this configuration
# (law-absence-needs-a-positive-control).
# =======================================================================================
echo
echo "a pass over watchers with nothing to say"
reset
notify 3600; rc=$?
is "an empty log escalates nothing"                    "0" "$rc"
is "and nothing was asked"                             "0" "$(asks)"
is "the run says so with no error"                     "" "$(cat "$TMP/err")"
# The control itself: the same command, given something to find, DOES find it. Proved in full
# below; here it is enough that the manifest and the filter are usable.
printf '%s one\n' "$FILTER" >> "$A"
# TWO PASSES, BECAUSE ONE CANNOT FIRE. The first sighting of an event only starts its clock,
# whatever the threshold — an event is escalated for having gone unread for a while, and the
# harness cannot know how long a line it has just seen for the first time has been sitting
# there. A control that ran one pass would prove nothing about the second.
notify 0; notify 0; rc=$?
is "and the same command finds a matured event when there is one" "1" "$rc"
is "which is the one ask so far"                       "1" "$(asks)"

# =======================================================================================
# The clock. An event is escalated for having gone unread for a WHILE, so the first sighting
# must not fire — otherwise the threshold is decoration and every event pages somebody.
# =======================================================================================
echo
echo "the clock starts on first sighting and matures"
reset
printf 'ANSWERED — a shipped-vocabulary line that this filter does not match\n' >> "$A"
printf '%s the operator answered\n' "$FILTER" >> "$A"
notify 3600; rc=$?
is "the first sighting does not escalate"              "0" "$rc"
is "and asks nothing"                                  "0" "$(asks)"
# THE CLOCK IS KEYED ON THE POSITION OF THE OLDEST ACTIONABLE LINE, which here is the second
# line in the log — the first is a decoy in the SHIPPED vocabulary, and a filter written into
# the code rather than read from configuration would put a 1 here.
is "and it records where the oldest actionable line is" "2" \
   "$(cut -d' ' -f1 "$RUN/watchd/alpha.pending" 2>/dev/null)"

# A LATER PASS, STILL BEFORE THE THRESHOLD, IS STILL SILENT — and this is the only assertion
# in the file that reads the threshold at all. Every other "not yet" here is a FIRST sighting,
# which the stamp-and-continue branch answers on its own without consulting the age: deleting
# `[ "$age" -ge "$SPIRA_NOTIFY_AGE" ]` outright left the suite at 75/75 green, on a mechanism
# that then escalated on the second pass — five minutes rather than the configured thirty.
# That is the false-alarm direction (law-alerts-must-be-actionable), so it is the direction
# worth a positive control. The clock is stamped here and the event is zero seconds into 3600.
notify 3600; rc=$?
is "a later pass before the threshold is still silent"  "0" "$rc"
is "and still asks nothing"                            "0" "$(asks)"

mature_pending
notify 0; rc=$?
is "once it has matured, it escalates"                 "1" "$rc"
is "exactly once"                                      "1" "$(asks)"
has "and the ask carries the event itself"             "$(cat "$ASKS")" "$FILTER the operator answered"
has "and a default, because an ask without one makes the operator decide from scratch" \
    "$(cat "$ASKS")" "--default"
hasnt "the decoy line is not in the evidence"          "$(cat "$ASKS")" "shipped-vocabulary"

# =======================================================================================
# ONCE PER BACKLOG. The acceptance criterion, and the property that decides whether this
# timer is an alert or a siren.
# =======================================================================================
echo
echo "a standing backlog is escalated once, not once per pass"
# THE PASSES ARE SPACED, and that second of wall clock is the whole test. Run back to back
# they land in the same second, so anything volatile the suppression key happened to contain
# would still match and the suite would pass on a timer that asks again every five minutes —
# which is the failure this section exists to catch, not a hypothetical one: the age WAS in
# the key at one point and this section, unspaced, said nothing.
sleep 1
notify 1; is "a second pass over the same backlog escalates again"       "1" "$?"
sleep 1
notify 1; is "and a third"                                               "1" "$?"
is "but no further ask was raised"                     "1" "$(asks)"
# A LINE ARRIVING BEHIND A STANDING ONE IS THE SAME BACKLOG. The oldest unread event is still
# the oldest unread event; a key that moved with the log would let one watcher ask forever.
# This is also what makes the whole thing loop-safe: raising an ask writes a bead, a watcher
# may emit a line about that bead, and that line lands behind the event being reported.
printf '%s and again\n' "$FILTER" >> "$A"
notify 1
is "an event arriving behind it does not ask again"    "1" "$(asks)"
has "though the report does count it"                  "$(cat "$TMP/out")" "2 actionable event(s)"

# =======================================================================================
# The cursor. Escalating is an EXTRA copy of the event, never a substitute for it.
# =======================================================================================
echo
echo "notify does not mark anything read"
unread="$(wd status | awk '$1=="alpha"{print $4}')"
is "the unread count is untouched by three escalating passes" "3" "$unread"
out="$(wd drain alpha)"
has "and a reader latching afterwards still gets the event" "$out" "$FILTER the operator answered"

# =======================================================================================
# Draining clears the condition — and the NEXT backlog is heard.
# =======================================================================================
echo
echo "draining clears it"
notify 1; rc=$?
is "with the log drained there is nothing to escalate"  "0" "$rc"
is "the clock is gone"  "no" "$([ -e "$RUN/watchd/alpha.pending" ] && echo yes || echo no)"
# THE SUPPRESSION MUST END WITH THE CONDITION. Left in place, a backlog that recurred
# identically would match the old fingerprint and reach nobody at all.
is "and so is the suppression" "no" \
   "$([ -e "$RUN/watchd/notify.escalated" ] && echo yes || echo no)"
printf '%s a new one, hours later\n' "$FILTER" >> "$A"
notify 0
mature_pending
notify 0; is "a new backlog escalates"                  "1" "$?"
is "and it is a second ask, not a suppressed one"       "2" "$(asks)"

# AND THE SAME EVENT AGAIN, BYTE FOR BYTE — the case the file check above cannot see, and
# the one that costs something. The log is rotated and the cursor reset, so the recurrence is
# IDENTICAL to the one already escalated: same watcher, same position, same line. A
# suppression that outlived its condition matches that fingerprint and swallows the second
# occurrence entirely, and nothing about it looks wrong from outside.
reset
printf '%s the very same line\n' "$FILTER" >> "$A"
notify 0; mature_pending; notify 0
is "the first occurrence is escalated"                  "1" "$(asks)"
wd drain alpha >/dev/null; notify 0
: > "$A"; rm -f "$RUN/watchd/alpha.cursor"
printf '%s the very same line\n' "$FILTER" >> "$A"
notify 0; mature_pending; notify 0
is "and an identical one, after the first was cleared, is heard again" "2" "$(asks)"

# =======================================================================================
# Only actionable lines wake anybody. A watcher's log is mostly progress, and paging somebody
# because a watcher was busy is the false alarm that makes the real one unreadable.
# =======================================================================================
echo
echo "progress is not an escalation"
reset
for i in $(seq 1 50); do printf 'pass %s: LANDED ok, FAIL none, ANSWERED nothing\n' "$i" >> "$B"; done
notify 3600; notify 0; rc=$?
is "fifty lines of shipped-vocabulary progress escalate nothing" "0" "$rc"
is "and ask nothing"                                   "0" "$(asks)"
is "no clock was even started"  "no" "$([ -e "$RUN/watchd/beta.pending" ] && echo yes || echo no)"

# =======================================================================================
# A second watcher going stale is NEW information, and does ask again.
# =======================================================================================
echo
echo "a second watcher is new information"
reset
printf '%s alpha needs you\n' "$FILTER" >> "$A"
notify 0; mature_pending; notify 0
is "the first watcher asks"                            "1" "$(asks)"
printf '%s beta needs you too\n' "$FILTER" >> "$B"
notify 0; mature_pending; notify 0; rc=$?
is "and the second one asks as well"                   "2" "$(asks)"
is "still reporting the condition"                     "1" "$rc"
has "with both watchers in the report"                 "$(cat "$TMP/out")" "beta"
has "and the first still there"                        "$(cat "$TMP/out")" "alpha"

# =======================================================================================
# The evidence is bounded. It is read in a pane, so a backlog of hundreds would bury the
# decision it is evidence for — but nothing is hidden, only deferred, and it says so.
# =======================================================================================
echo
echo "a large backlog is summarised, and says how much it left out"
reset
for i in $(seq 1 30); do printf '%s event %s\n' "$FILTER" "$i" >> "$A"; done
notify 0
mature_pending
notify 0
out="$(cat "$TMP/out")"
has "the count is stated in full"                      "$out" "30 actionable event(s)"
n="$(grep -c "    $FILTER event" <<< "$out" || true)"
is "but the listing is capped"  "yes" "$([ "$n" -lt 30 ] && [ "$n" -gt 0 ] && echo yes || echo no)"
has "and it names the command that hands over the rest" "$out" "watchd.sh drain alpha"

# =======================================================================================
# A WATCHER THIS INSTALLATION HAS NOT GOT IS NOT A BACKLOG. An optional row whose key is
# unset renders as kind `off`, and nothing writes a log for it — so there is nothing standing
# unread and nobody to wake about it. This is not a hypothetical row: the shipped manifest
# carries one, so it is what `notify` meets on every pass of a default installation.
#
# THE ASSERTION IS PAIRED WITH A LIVE WATCHER ON PURPOSE. "No ask was raised" is also what a
# pass that refused the whole manifest looks like, and the two are the same exit code from
# outside — so the same pass must still find the real backlog next to it
# (law-absence-needs-a-positive-control).
# =======================================================================================
echo
echo "a watcher that is not installed has no backlog"
reset
OFFMAN="$TMP/watchers-off"
{ printf '?ghost|log|@SPIRA_VIEW@\n'; printf 'alpha|log|%s\n' "$A"; } > "$OFFMAN"
notify 0 "$OFFMAN"; rc=$?
is "an off row is not an event"                        "0" "$rc"
is "and asks nothing"                                  "0" "$(asks)"
is "and does not stumble over its unnamed log"         "" "$(cat "$TMP/err")"
is "nor start a clock for a watcher that cannot tick"  "no" \
   "$([ -e "$RUN/watchd/ghost.pending" ] && echo yes || echo no)"
# The control: the very same pass over the very same manifest still finds a real one.
printf '%s alpha still needs you\n' "$FILTER" >> "$A"
notify 0 "$OFFMAN"; notify 0 "$OFFMAN"; rc=$?
is "while a watcher that IS installed is still heard"  "1" "$rc"
is "and escalated"                                     "1" "$(asks)"
hasnt "with the uninstalled row absent from the report" "$(cat "$TMP/out")" "ghost"

# =======================================================================================
# Refusing rather than passing. Each of these is a way for the check itself to be broken, and
# every one of them must be distinguishable from "nothing is waiting".
# =======================================================================================
echo
echo "a check that cannot look refuses"
reset
printf '%s something\n' "$FILTER" >> "$A"
BAD="$TMP/bad-manifest"
printf 'alpha|log|%s\nthis row is not a row\n' "$A" > "$BAD"
notify 0 "$BAD"; rc=$?
is "a malformed manifest is neither a pass nor a finding" "3" "$rc"
is "and nothing is asked on the strength of it"        "0" "$(asks)"
has "and it says which line"                           "$(cat "$TMP/err")" "this row is not a row"

FILTER_OVERRIDE="" notify 0; rc=$?
is "an empty filter is refused, not read as matching everything" "3" "$rc"
is "and asks nothing"                                  "0" "$(asks)"
has "and names the escape hatch"                       "$(cat "$TMP/err")" "SPIRA_ACTIONABLE"

notify "half an hour"; rc=$?
is "a threshold that is not a number is refused"       "3" "$rc"
is "rather than silently never firing"                 "0" "$(asks)"
has "and says what it must be"                         "$(cat "$TMP/err")" "whole number of seconds"

notify 0 "$TMP/no-such-manifest"; rc=$?
is "a manifest that is not there is refused"           "3" "$rc"

# =======================================================================================
# A broken channel must not CONSUME the finding. This is the ordering that matters: stamping
# before the ask was accepted would mark the one notification this backlog will ever produce
# as delivered, and the retry that would have carried it never happens.
# =======================================================================================
echo
echo "a refused escalation is retried, not swallowed"
reset
printf '%s the channel is down\n' "$FILTER" >> "$A"
notify 0
mature_pending
NOTIFY_REFUSE=1 notify 0; rc=$?
is "an escalation path that refuses is a broken mechanism, not a clean pass" "3" "$rc"
is "and nothing was recorded as asked"                 "0" "$(asks)"
is "so no suppression was written" "no" \
   "$([ -e "$RUN/watchd/notify.escalated" ] && echo yes || echo no)"
notify 0; rc=$?
is "once the path is repaired the same backlog is delivered" "1" "$rc"
is "and the event finally reaches somebody"            "1" "$(asks)"
has "carrying what it was holding all along"           "$(cat "$ASKS")" "the channel is down"

reset
env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$CONF" SPIRA_WATCHERS="$MAN" \
    SPIRA_ACTIONABLE="$FILTER" SPIRA_NOTIFY_AGE=0 \
    bash "$NOMAIL/watchd.sh" notify >/dev/null 2>"$TMP/err"
printf '%s nobody to tell\n' "$FILTER" >> "$A"
env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$CONF" SPIRA_WATCHERS="$MAN" \
    SPIRA_ACTIONABLE="$FILTER" SPIRA_NOTIFY_AGE=0 \
    bash "$NOMAIL/watchd.sh" notify >/dev/null 2>"$TMP/err"
env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$CONF" SPIRA_WATCHERS="$MAN" \
    SPIRA_ACTIONABLE="$FILTER" SPIRA_NOTIFY_AGE=0 \
    bash "$NOMAIL/watchd.sh" notify >/dev/null 2>"$TMP/err"; rc=$?
is "no escalation path at all is a broken mechanism too" "3" "$rc"
has "and it says the events reach nobody"              "$(cat "$TMP/err")" "reach nobody"

# =======================================================================================
# `notify` is a verb of its own and takes nothing. A typo taken as an argument would be
# ignored in silence, which for a timer means running the wrong thing forever.
# =======================================================================================
echo
echo "the verb takes no arguments"
env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$CONF" SPIRA_WATCHERS="$MAN" \
    SPIRA_ACTIONABLE="$FILTER" NOTIFY_LOG="$ASKS" \
    bash "$CLONE/spira/watchd.sh" notify --all >/dev/null 2>"$TMP/err"
is "an unexpected argument is refused"                 "3" "$?"
has "with the usage"                                   "$(cat "$TMP/err")" "usage: watchd.sh notify"

# =======================================================================================
# The unit. law-fence-loops-on-shared-hardware: anything that polls gets a quota BEFORE it is
# enabled, and no path in a unit may be anything but configuration.
# =======================================================================================
echo
echo "the unit is fenced, configured, and enabled"
mkdir -p "$TMP/render-home"
RCONF="$TMP/render.conf"
# SPIRA_PROD pinned to empty: render() falls back to SPIRA_HOME ($CLONE/spira), so the
# ExecStart path comes from the clone and not a derived $WORKSPACES/clone-prod path that
# does not exist in the test tree (sp-82jo added the executability fence; sp-kteb).
printf 'SPIRA_RUN = %s\nSPIRA_COCKPIT = %s\nSPIRA_WATCHERS = %s\nSPIRA_PROD = \n' \
    "$RUN" "$COCKPIT" "$MAN" > "$RCONF"
rendered="$(env -i HOME="$TMP/render-home" PATH="$PATH" SPIRA_CONF="$RCONF" \
    bash "$CLONE/systemd/install.sh" --render 2>/dev/null)"
is "the renderer produced units" "yes" "$([ -n "$rendered" ] && echo yes || echo no)"
# sp-fo38 added per-instance unit suffixes; extract the prod-instance name (default).
svc="$(awk '/^===== spira-watch-notify-prod.service =====$/{f=1;next} /^===== /{f=0} f' <<< "$rendered")"
tmr="$(awk '/^===== spira-watch-notify-prod.timer =====$/{f=1;next} /^===== /{f=0} f' <<< "$rendered")"
is "the service is rendered"  "yes" "$([ -n "$svc" ] && echo yes || echo no)"
is "and so is the timer"      "yes" "$([ -n "$tmr" ] && echo yes || echo no)"
has "it runs the dispatcher's notify verb"      "$svc" "$CLONE/spira/watchd.sh notify"
has "it is CPU-fenced"                          "$svc" "CPUQuota="
has "and niced"                                 "$svc" "Nice="
# A STANDING BACKLOG IS NOT A UNIT FAILURE. Exit 1 means the condition was found and the
# decision is already in front of the operator; a unit left permanently red is one whose next
# genuine failure nobody looks at.
has "a finding does not leave the unit red"     "$svc" "SuccessExitStatus=1"
hasnt "no placeholder survives into the unit"   "$svc" "@"
hasnt "and none into the timer"                 "$tmr" "@"
# NO PATH IS HARDCODED. Every absolute path in the rendered unit must lie under a value that
# came from the config above, which pins both to non-defaults.
stray=""
while IFS= read -r p; do
    [ -n "$p" ] || continue
    case "$p" in "$CLONE/spira"/*|"$RUN"/*) ;; *) stray="$stray $p" ;; esac
done < <(sed 's|file://|file:|' <<< "$svc" | grep -oE '[=:]/[^ ]+' | sed 's/^[=:]//')
is "every path in it came from configuration"   "" "$stray"

# THE PERIOD MUST BE WELL UNDER THE THRESHOLD, or the granularity with which staleness is
# noticed doubles the wait the threshold was set to allow.
period="$(sed -n 's/^OnUnitActiveSec=\([0-9]*\)min$/\1/p' <<< "$tmr")"
default_age="$(env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$TMP/nonexistent" \
    bash -c ". '$CLONE/spira/conf.sh'; printf '%s' \"\$SPIRA_NOTIFY_AGE\"")"
is "the timer states a period in minutes" "yes" "$([ -n "$period" ] && echo yes || echo no)"
is "the threshold has a default"          "yes" "$([ -n "$default_age" ] && echo yes || echo no)"
is "and a pass happens several times inside it" "yes" \
   "$([ "$(( period * 60 * 3 ))" -le "$default_age" ] && echo yes || echo no)"

# THE TIMER IS ENABLED. A unit that is installed and never started is a mechanism that exists
# only in the repository — which is the shape of every defect this whole design is about.
STUB="$TMP/stub"; mkdir -p "$STUB"
# The stub logs every non-query systemctl call so the assertions below can grep it.
# is-active returns "inactive" until a unit is enabled (enable --now) or restarted, then
# "active" — this is the correct sequence for a fresh install: units do not exist before
# install.sh runs, so the ENABLE loop uses "enable --now" rather than "enable"+"restart".
# After that the sp-syub end-state check calls is-active for every ENABLE unit and expects
# "active", which the stateful stub provides once the unit has been enabled (sp-kteb).
# list-* calls (list-units, list-timers, list-unit-files) are informational and not asserted.
cat > "$STUB/systemctl" <<EOF
#!/usr/bin/env bash
mkdir -p "$TMP/active"
case "\$*" in
    *"is-active"*)
        _u="\${*##* }"
        [ -f "$TMP/active/\$_u" ] && printf 'active\n' || printf 'inactive\n'
        ;;
    *"list-"*) : ;;
    *)
        printf '%s\n' "\$*" >> "$TMP/systemctl.log"
        case "\$*" in
            *"enable --now "*) touch "$TMP/active/\${*##*enable --now }" ;;
            *"restart "*)      touch "$TMP/active/\${*##*restart }" ;;
        esac
        ;;
esac
exit 0
EOF
printf '#!/usr/bin/env bash\nexit 0\n' > "$STUB/loginctl"
chmod +x "$STUB/systemctl" "$STUB/loginctl"
IHOME="$TMP/ihome"; mkdir -p "$IHOME"
: > "$TMP/systemctl.log"
# Reached through SPIRA_PATH and not PATH: conf.sh REPLACES PATH outright, so a directory
# handed in through the environment is gone before install.sh runs anything.
# SPIRA_PROD points to the real harness so the sp-82jo executability fence finds real scripts.
# SPIRA_INSTALL_FORCE=1 bypasses the sp-mlcd landref check; the clone is not a git repo.
# SPIRA_COCKPIT points to the real cockpit dir so cockpit-ensure.service's ExecStart target
# (layout.sh) resolves to an executable file; the synthetic $COCKPIT dir has none (sp-kteb).
printf 'SPIRA_RUN = %s\nSPIRA_COCKPIT = %s\nSPIRA_WATCHERS = %s\nSPIRA_PATH = %s\nSPIRA_PROD = %s\n' \
    "$RUN" "$ROOT/cockpit" "$MAN" "$STUB" "$HERE" > "$TMP/install.conf"
# SPIRA_HOME is set so @SPIRA_HOME@ units (auron, watch-refresh) point at real scripts;
# conf.sh otherwise derives it from the clone, where only conf.sh and watchd.sh exist.
env -i HOME="$IHOME" PATH="$STUB:$PATH" SPIRA_CONF="$TMP/install.conf" \
    SPIRA_INSTALL_FORCE=1 SPIRA_HOME="$HERE" \
    bash "$CLONE/systemd/install.sh" > "$TMP/install.out" 2>&1
log="$(cat "$TMP/systemctl.log")"
has "the install ran"                           "$log" "daemon-reload"
# sp-fo38 added per-instance unit suffixes; the default instance is "prod".
has "and enabled the notify timer"              "$log" "enable --now spira-watch-notify-prod.timer"
# The .service behind a .timer is started BY the timer; enabling it as well would also run it
# once at boot, outside the schedule.
hasnt "but not the service behind it"           "$log" "enable --now spira-watch-notify-prod.service"
has "and the unit files are installed"          "$(ls "$IHOME/.config/systemd/user")" "spira-watch-notify-prod.timer"


# =======================================================================================
# THE PRODUCER'S OWN CLOCK. First sighting is the wrong clock for a backlog written all at
# once when a dead watcher recovers: answers given during the outage are the most overdue
# and would be handed a fresh grace period. A `[<ISO8601>]` prefix is believed instead.
# =======================================================================================
echo
echo "a line that carries its own timestamp is dated by it, not by when it was seen"
# ago <seconds> — a fixed-width UTC stamp that far in the past.
ago() { date -u -d "@$(( $(date -u +%s) - $1 ))" +%Y-%m-%dT%H:%M:%SZ; }

reset
printf '[%s] %s answered while the watcher was down\n' "$(ago 7200)" "$FILTER" >> "$A"
notify 3600; rc=$?
is "a stamp already past the threshold escalates on FIRST sighting" "1" "$rc"
is "and raises exactly one ask"                        "1" "$(asks)"
has "and the age reported is the stamp's, not zero"    "$(cat "$TMP/out")" "2h"

# THE FALSE-ALARM DIRECTION, which is the one worth a control: reading the stamp must not
# collapse into "always fire on sight".
reset
printf '[%s] %s answered a moment ago\n' "$(ago 5)" "$FILTER" >> "$A"
notify 3600; rc=$?
is "a fresh stamp does not escalate"                   "0" "$rc"
is "and asks nothing"                                  "0" "$(asks)"

# The oldest standing stamp is the backlog's age; a newer line behind it cannot mask it.
reset
printf '[%s] %s the old one\n' "$(ago 7200)" "$FILTER" >> "$A"
printf '[%s] %s the new one\n' "$(ago 5)" "$FILTER" >> "$A"
notify 3600
is "the oldest stamp decides, not the newest"          "1" "$(asks)"

# THE FALLBACK SURVIVES. Unstamped watchers — a `log` row someone else writes — must still
# be dated from first sighting, or every one of their events pages on sight.
reset
printf '%s no stamp at all\n' "$FILTER" >> "$A"
notify 0; rc=$?
is "an unstamped line still waits for a second sighting" "0" "$rc"
is "and asks nothing yet"                              "0" "$(asks)"
notify 0
is "and escalates on the next pass, as before"         "1" "$(asks)"

# A stamp that will not parse is not a stamp. Falling back beats trusting a junk date, in
# either direction: `date` failing must not fire, and must not wedge the pass.
reset
printf '[not-a-timestamp] %s malformed\n' "$FILTER" >> "$A"
notify 3600; rc=$?
is "a malformed stamp falls back to first sighting"    "0" "$rc"
is "and does not error"                                "" "$(cat "$TMP/err")"

# =======================================================================================
# THE OTHER HALF: A WATCHER THAT HAS STOPPED PRODUCING. The events half reports lines that
# WERE written, so it is blind by construction to the failure that stops writing. A unit
# systemd respawned 108 times reached nobody for hours while `status` said DEGRADED.
# =======================================================================================
echo
echo "a watcher that has been unwell too long is escalated"

# A daemon row, and a stub systemd that answers for it. State, restart count and silence are
# read from files so a test can move them without rebuilding the stub.
GTARGET="$TMP/gamma-watcher"; printf '#!/bin/sh\nsleep 99\n' > "$GTARGET"; chmod +x "$GTARGET"
GHEALTH="$TMP/gamma-health"; printf 'exit 0\n' > "$GHEALTH"
MAND="$TMP/watchers-daemon"
printf 'gamma|daemon|%s|bash %s\n' "$GTARGET" "$GHEALTH" > "$MAND"

# INJECTED THROUGH SPIRA_PATH, NOT PATH. conf.sh rebuilds PATH from SPIRA_PATH plus a fixed
# tail, so a stub merely prepended to PATH is discarded and the box's real systemctl answers
# instead — which under `env -i` has no bus, returns nothing, and makes "no unwell watcher"
# pass for a reason that has nothing to do with the code.
STUBBIN="$TMP/stubbin"; mkdir -p "$STUBBIN"
SC_STATE="$TMP/sc-state"; SC_NR="$TMP/sc-nr"; SC_SILENT="$TMP/sc-silent"
printf 'active\n' > "$SC_STATE"; printf '0\n' > "$SC_NR"; : > "$SC_SILENT"
cat > "$STUBBIN/systemctl" <<'SC'
#!/usr/bin/env bash
[ -s "$SC_SILENT" ] && exit 1
state="$(cat "$SC_STATE" 2>/dev/null)"; [ -n "$state" ] || state=active
nr="$(cat "$SC_NR" 2>/dev/null)"; [ -n "$nr" ] || nr=0
units=(); for a in "$@"; do case "$a" in *.service) units+=("$a") ;; esac; done
for a in "$@"; do
  case "$a" in
    show) for u in "${units[@]}"; do printf 'Id=%s\nActiveState=%s\nNRestarts=%s\n\n' "$u" "$state" "$nr"; done; exit 0 ;;
  esac
done
exit 0
SC
chmod +x "$STUBBIN/systemctl"

# notify_d <age> — the same command over the daemon manifest. SPIRA_ACTIONABLE stays pinned
# so the events half cannot match a line by accident and answer for the health half.
notify_d() {
    env -i HOME="$TMP/home" PATH="$PATH" SPIRA_PATH="$STUBBIN" SPIRA_CONF="$CONF" \
        SPIRA_WATCHERS="$MAND" SPIRA_ACTIONABLE="$FILTER" \
        SPIRA_NOTIFY="$TMP/notify.sh" SPIRA_NOTIFY_AGE="$1" \
        SC_STATE="$SC_STATE" SC_NR="$SC_NR" SC_SILENT="$SC_SILENT" \
        NOTIFY_LOG="$ASKS" \
        bash "$CLONE/spira/watchd.sh" notify > "$TMP/out" 2> "$TMP/err"
}
mature_unhealthy() {
    local f at
    for f in "$RUN/watchd/"*.unhealthy; do
        [ -f "$f" ] || continue
        read -r at < "$f"; printf '%s\n' "$(( at - 3600 ))" > "$f"
    done
}
gamma_says() { mkdir -p "$RUN/watchd"; printf '%s\n' "$1" >> "$RUN/watchd/gamma.log"; }

# THE POSITIVE CONTROL. A healthy daemon must escalate nothing, or every assertion below is
# satisfied by a command that pages about everything.
reset
printf 'active\n' > "$SC_STATE"
notify_d 3600; rc=$?
is "a healthy daemon escalates nothing"                "0" "$rc"
is "and asks nothing"                                  "0" "$(asks)"
is "with no error"                                     "" "$(cat "$TMP/err")"

# Now the same watcher, dead. The first sighting only starts the clock: a unit restarting for
# ten seconds is not yet a fault, and paging on sight is the false-alarm direction.
reset
printf 'activating\n' > "$SC_STATE"; printf '108\n' > "$SC_NR"
gamma_says 'cockpit watcher already running'
notify_d 3600; rc=$?
is "a watcher just gone unwell is not escalated yet"   "0" "$rc"
is "and asks nothing"                                  "0" "$(asks)"
is "but the clock has started"                         "1" \
   "$(ls "$RUN/watchd/gamma.unhealthy" >/dev/null 2>&1 && echo 1 || echo 0)"

mature_unhealthy
notify_d 3600; rc=$?
is "once it has been unwell for the threshold, it escalates" "1" "$rc"
is "exactly once"                                      "1" "$(asks)"
# THE DISCRIMINATING FACTS. The RESTARTS column counts only the restarts watch-refresh makes
# for a code change, so a unit systemd is respawning shows a still number there; the ask has
# to carry systemd's own count. The last line written is what named the copy in the way.
has "and the ask carries systemd's restart count"      "$(cat "$ASKS")" "108 time(s)"
has "and the last line the watcher wrote"              "$(cat "$ASKS")" "already running"
has "and a default that says what to do"               "$(cat "$ASKS")" "--default"
has "which names the second-copy case"                 "$(cat "$ASKS")" "second copy"

sleep 1
notify_d 3600
is "a standing fault does not ask again"               "1" "$(asks)"

# RECOVERY CLEARS THE CLOCK, and a recurrence is heard. Suppression that outlived its
# condition would swallow the second outage entirely.
printf 'active\n' > "$SC_STATE"
notify_d 3600; rc=$?
is "a recovered watcher escalates nothing"             "0" "$rc"
is "and its clock is cleared"                          "0" \
   "$(ls "$RUN/watchd/gamma.unhealthy" >/dev/null 2>&1 && echo 1 || echo 0)"
printf 'failed\n' > "$SC_STATE"
notify_d 3600; mature_unhealthy; notify_d 3600
is "a second outage is heard"                          "2" "$(asks)"

# AN ACTIVE UNIT STILL FAILS ITS OWN PROBE. Unit state is not health: the blind watcher that
# prompted the health column was running perfectly and reading a database retired underneath
# it. Without this the whole half collapses to `is-active`.
reset
printf 'active\n' > "$SC_STATE"
printf 'echo the state file names no bead of ours >&2; exit 1\n' > "$GHEALTH"   # a probe's reason is its STDERR
notify_d 3600; mature_unhealthy; notify_d 3600
is "an active unit that fails its probe is escalated"  "1" "$(asks)"
has "and the ask carries the probe's own words"        "$(cat "$ASKS")" "no bead of ours"
printf 'exit 0\n' > "$GHEALTH"

# NO ANSWER FROM systemd IS NOT A FAULT IN THE WATCHER. Without a user manager at all every
# watcher would otherwise be reported dead (law-absence-needs-a-positive-control).
reset
printf 'x\n' > "$SC_SILENT"
notify_d 0; notify_d 0
is "a box with no systemd answer reports no unwell watcher" "0" "$(asks)"
is "and starts no clock it cannot justify"             "0" \
   "$(ls "$RUN/watchd/gamma.unhealthy" >/dev/null 2>&1 && echo 1 || echo 0)"
: > "$SC_SILENT"

# THE TWO HALVES ARE INDEPENDENT. They share a timer and nothing else: one stamp file would
# let whichever escalated last erase the other's fingerprint, and a noisy watcher would hide
# a dead one for as long as it kept talking.
reset
printf 'activating\n' > "$SC_STATE"
gamma_says "$FILTER an unread event"
notify_d 0; mature_pending; mature_unhealthy; notify_d 0; rc=$?
is "an unread backlog and a dead watcher both escalate" "1" "$rc"
is "as two asks, not one"                              "2" "$(asks)"
is "from two separate suppression stamps"              "1" \
   "$(ls "$RUN/watchd/notify.escalated" "$RUN/watchd/notify-health.escalated" >/dev/null 2>&1 && echo 1 || echo 0)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
