#!/usr/bin/env bash
#
# test-event.sh — the outcome stream: that it records, that it does not flood, and that
# every kind the harness emits is one the emitter will accept.
#
#   ./test-event.sh
#
# Spira's outcomes used to exist only as text. The health pane scraped three log files for
# landings, reopenings, poisonings, reclaims and claims, sorted them and kept four — so
# everything below the fourth line was not aged out, it was never stored, and "how many times
# did a bead reopen" was unanswerable without grepping a log that rotates.
#
# The half of that fix with no second reader is the RATE LIMIT. An emitter that floods is not
# a milder failure than one that is silent: a stream nobody can read is the log line it
# replaced, and a hot retry loop can produce dozens of reclaims per hour. So the negative
# cases here — a steady state emits nothing, a storm is one row — carry more weight than
# the positive ones, and both must hold at once: a suppressor with no counter would pass the
# storm case by recording nothing at all.
#
# Events are informational and go to events.log, not the operator mailbox. Operator asks
# (question/decision mails) are sent directly by the callers that have the context to write
# them properly (claims, reopens, landings belong in the log; the mailbox holds only decisions).
#
# The taxonomy and call-site wiring are pure source greps and live in
# test-event-taxonomy.sh (T0) instead — coverage-map row 20,
# docs/test-plan/cockpit-observability.md.
#
# defect: sp-gvm
# tier: T1
# covers: spira/lib.sh UC-cockpit-observability-20
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
SH="$TMP/spira"; RUN="$TMP/run"; mkdir -p "$SH" "$RUN"
# conf.sh travels with lib.sh, which refuses to run without it.
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"

# In a subshell, never sourced here: lib.sh overwrites PATH outright, as it must to run under
# systemd, and a suite that inherited that would be testing the harness's PATH as well.
cat > "$TMP/emit" <<'E'
#!/usr/bin/env bash
set -uo pipefail
. "$SPIRA_HOME/lib.sh"
spira_event "$@"
E
chmod +x "$TMP/emit"

# SPIRA_CONF=/nonexistent: this suite asserts what the code does, and the operator's own
# configuration is not part of that (law-gates-run-in-a-clean-environment).
emit() {   # emit <kind> <target|-> <title> [detail]
    SPIRA_CONF=/nonexistent SPIRA_HOME="${HOME_OVERRIDE:-$SH}" SPIRA_REPO="$TMP" SPIRA_RUN="$RUN" \
    SPIRA_DB=/nonexistent-spira-db \
    SPIRA_EVENT_COOLDOWN="${COOLDOWN:-3600}" \
    SPIRA_NOW="${NOW:-$(date -u +%s)}" \
        bash "$TMP/emit" "$@" 2>&1
}
emitted()  { cat "$RUN/events.log" 2>/dev/null; }
# Count non-empty lines in events.log — one per recorded event.
# wc -l always exits 0; the || handles a missing file (grep would exit 2 with no output).
rows()     { [ -f "$RUN/events.log" ] && wc -l < "$RUN/events.log" || echo 0; }
fresh()    { rm -rf "$RUN/events" "$RUN/events.log"; }

echo "test-event.sh"

# --------------------------------------------------------------------------------------
echo
echo "the positive control — an outcome reaches the log whole"
# --------------------------------------------------------------------------------------
fresh
out="$(emit bead.landed sp-x "landed spira/sp-x on brain's main" "merged as abc1234")"
is   "a first outcome is recorded"        "1"            "$(rows)"
want "with the kind"                      "kind: bead.landed" "$(emitted)"
want "the bead it happened to"            "target: sp-x" "$(emitted)"
want "the title as written"               "landed spira/sp-x on brain's main" "$(emitted)"
want "and the detail beside it"           "merged as abc1234" "$(emitted)"
nowant "and never as an email"            "mail.sh"      "$out"

# An outcome about the plan rather than a bead carries "-" in the target column, not the
# word "plan": the column is for bead ids only and a literal string would filter incorrectly.
fresh
emit spira.note - "the plan moved" >/dev/null
want "a plan-level outcome records a dash for target" "target: -" "$(emitted)"

# --------------------------------------------------------------------------------------
echo
echo "the rate limit — a steady state is not news"
# --------------------------------------------------------------------------------------
fresh
emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim 1" >/dev/null
is "the first reclaim is recorded" "1" "$(rows)"
emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim 2" >/dev/null
is "a repeat inside the window is not" "1" "$(rows)"

# THE STORM: 26 attempts in three hours was the real case that shaped this.
fresh
for n in $(seq 1 26); do emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim $n" >/dev/null; done
is "a 26-reclaim storm is one row, not 26" "1" "$(rows)"

# ...AND THE CHECK COULD HAVE COUNTED HIGHER. A suppressor that recorded nothing at all would
# pass the case above; the same loop over distinct beads must produce a row each, because the
# window is per (kind, bead) and a real fleet of reclaims is real news.
fresh
for n in $(seq 1 6); do emit branch.reclaimed "sp-storm$n" "reclaimed sp-storm$n" >/dev/null; done
is "six different beads reclaiming is six rows" "6" "$(rows)"

# The pair is (kind, target), so a bead that lands and is then reopened says both things.
fresh
emit bead.landed   sp-y "landed sp-y"   >/dev/null
emit bead.reopened sp-y "reopened sp-y" >/dev/null
is "a second KIND on the same bead is not a repeat" "2" "$(rows)"

# --------------------------------------------------------------------------------------
echo
echo "suppressed is not dropped — the count rides out on the next one"
# --------------------------------------------------------------------------------------
# A window that expires with a run of suppressions behind it must SAY so. A panel that
# renders a storm as one quiet row is a check reporting all-clear on the thing it exists to
# show (law-alerts-must-be-actionable).
#
# THE CLOCK IS INJECTED (SPIRA_NOW / NOW above), NOT SLEPT THROUGH. The rate limiter's
# window is real-seconds arithmetic on `now`, so an emit at a fixed NOW and one two seconds
# later exercise exactly the code path two real `sleep 2`s did, without spending them.
fresh
NOW=1000 COOLDOWN=1 emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim 1" >/dev/null
for n in 2 3 4; do NOW=1000 COOLDOWN=999 emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim $n" >/dev/null; done
is "three repeats are held" "1" "$(rows)"
NOW=1002 COOLDOWN=1 emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim 5" >/dev/null
is   "the expired window emits again"     "2"            "$(rows)"
want "carrying what it held back"         "+3 more since" "$(emitted)"

# And the window belongs to the FIRST emission, not the last suppression: refreshing it on
# every repeat is how a loop faster than the window goes permanently silent.
fresh
NOW=1000 COOLDOWN=2 emit branch.reclaimed sp-hot "reclaimed sp-hot — 1" >/dev/null
_hot_now=1000
for n in 2 3 4 5; do
    _hot_now=$((_hot_now + 1))
    NOW=$_hot_now COOLDOWN=2 emit branch.reclaimed sp-hot "reclaimed sp-hot — $n" >/dev/null
done
[ "$(rows)" -ge 2 ] && ok "a loop faster than the window still surfaces" \
                    || bad "a loop faster than the window still surfaces" "got $(rows) rows"

# --------------------------------------------------------------------------------------
echo
echo "bad inputs are refused — empty kind or title"
# --------------------------------------------------------------------------------------
fresh
out="$(emit "" sp-x "no kind")"; rc=$?
is "an outcome with no kind is refused" "1" "$rc"
is "and nothing is written"             "0" "$(rows)"
out="$(emit bead.landed sp-x "")"; rc=$?
is "an outcome with no title is refused" "1" "$rc"
is "and nothing is written for it either" "0" "$(rows)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
