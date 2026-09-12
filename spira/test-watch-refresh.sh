#!/usr/bin/env bash
#
# test-watch-refresh.sh — the staleness guard, and the cost of one pass.
#
#   ./test-watch-refresh.sh
#
# WHAT IT HOLDS. The guard exists because a watcher's silence and its blindness look the
# same from outside, so every property here is a way for the guard itself to be quietly
# useless:
#
#   1. A STALE WATCHER IS RESTARTED — and the check that says so is only believable because
#      the same fixture with nothing touched restarts nothing. The positive control runs
#      first (law-absence-needs-a-positive-control).
#   2. AN UNCHANGED WATCHER IS NEVER RESTARTED. This is the half that regresses silently: a
#      guard that restarts everything every minute passes the first property perfectly and
#      is a restart loop on a box running production.
#   3. A PASS COSTS TWO EXECS AND OPENS NO DATABASE. Asserted, not asserted about: the pass
#      runs with a PATH holding nothing but recording shims, so every external program it
#      reaches for is named, and anything unshimmed would fail rather than pass unseen.
#   4. IT FAILS LOUDLY OR NOT AT ALL. A malformed manifest, a `systemctl` that will not
#      answer, a start time in a format it cannot read — each restarts nothing and says so,
#      because a quiet pass is what a broken checker and a healthy fleet have in common.
#
# It needs no database and no beads server, and it never touches the box's own units: the
# only `systemctl` it can reach is a stub that records what it was asked to do.
#
# The units are named explicitly rather than by `systemd/*`: this suite asserts the CPUQuota
# and the 60s cadence out of those two files, so a change to either must run it, while a
# change to some other unit has no business selecting it.
#
# defect: sp-gys
# covers: spira/watch-refresh.sh systemd/spira-watch-refresh.service systemd/spira-watch-refresh.timer
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

REAL_STAT="$(command -v stat)"
REAL_MKDIR="$(command -v mkdir)"

# A harness tree that is NOT this checkout, so nothing here can read the operator's own
# configuration, units or watchers and report a pass it did not earn.
CLONE="$TMP/clone"
mkdir -p "$CLONE/spira" "$CLONE/cockpit"
cp "$HERE"/*.sh "$CLONE/spira/"
cp -r "$ROOT/systemd" "$CLONE/systemd"
# beads-push.sh and concierge.sh live at repo root (@SPIRA_REPO@); the ExecStart fence in
# install.sh requires them to be executable. SPIRA_REPO derives to $CLONE (parent of $CLONE/spira).
cp "$ROOT/beads-push.sh" "$ROOT/concierge.sh" "$CLONE/"

# EVERY CONFIGURED VALUE PINNED TO A NON-DEFAULT. SPIRA_COCKPIT would derive to
# $CLONE/cockpit and SPIRA_RUN to $CLONE/.runtime/spira; both are moved somewhere unrelated,
# so a path written into the code as a literal cannot pass by coincidence.
COCKPIT="$TMP/elsewhere/cockpit"; RUN="$TMP/elsewhere/run"
mkdir -p "$COCKPIT" "$RUN/watchd"
printf '#!/bin/sh\nsleep 3600\n' > "$COCKPIT/watch-answers.sh"
printf '#!/bin/sh\n: library\n'   > "$COCKPIT/db.sh"          # the library beside the target
printf '{}\n'                     > "$COCKPIT/state.json"     # what the watcher writes while running
printf 'event\n'                  > "$COCKPIT/scratch.log"
# layout.sh, moot-sweep.sh, and verify-asks.sh are ExecStart targets for
# cockpit-ensure.service, spira-moot-sweep.service, and spira-verify-asks.service
# respectively; the ExecStart fence in install.sh requires each to be executable.
printf '#!/bin/sh\n: stub\n'      > "$COCKPIT/layout.sh"
printf '#!/bin/sh\n: stub\n'      > "$COCKPIT/moot-sweep.sh"
printf '#!/bin/sh\n: stub\n'      > "$COCKPIT/verify-asks.sh"
chmod +x "$COCKPIT/watch-answers.sh" "$COCKPIT/layout.sh" "$COCKPIT/moot-sweep.sh" "$COCKPIT/verify-asks.sh"
CONF="$TMP/spira.conf"
printf 'SPIRA_COCKPIT = %s\nSPIRA_RUN = %s\n' "$COCKPIT" "$RUN" > "$CONF"

# A SECOND HARNESS COPY, so "the unit's ExecStart" can be told apart from "this harness's
# watchd.sh" — a box carrying a stale install points at exactly this, and it is the only
# reason the pass reads ExecStart out of systemd rather than deriving it.
OTHER="$TMP/other-harness"; mkdir -p "$OTHER"
printf '#!/bin/sh\n: dispatcher\n' > "$OTHER/watchd.sh"

MAN="$TMP/watchers"
cat > "$MAN" <<EOF
answers|daemon|@SPIRA_COCKPIT@/watch-answers.sh|
cron|log|@SPIRA_RUN@/somebody-elses.log
EOF

# Everything starts at one instant well in the past, so "newer than the process" is a fact
# the test sets rather than a race with the clock.
T0=1735689600
UNIT_START=$((T0 + 100))
NEWER=$((T0 + 200))
reset_mtimes() {
    touch -d "@$T0" "$CLONE/spira"/*.sh "$COCKPIT"/* "$OTHER"/watchd.sh "$CONF" "$MAN"
}

# ---- the stubs -------------------------------------------------------------------------
# `#!/bin/bash` and not `/usr/bin/env bash`: the pass runs with a PATH holding nothing but
# these, and `env` resolves its argument through PATH.
SHIM="$TMP/shim"; mkdir -p "$SHIM"
cat > "$SHIM/systemctl" <<EOF
#!/bin/bash
printf 'systemctl %s\n' "\$*" >> "\$WR_EXECLOG"
case "\$*" in
    *" show "*) while IFS= read -r l; do printf '%s\n' "\$l"; done < "\$WR_SHOW"
                exit "\${SYSTEMCTL_RC:-0}" ;;
    *restart*)  printf '%s\n' "\$*" >> "\$WR_ACT"; exit "\${RESTART_RC:-0}" ;;
esac
exit 0
EOF
cat > "$SHIM/stat" <<EOF
#!/bin/bash
printf 'stat %s\n' "\$*" >> "\$WR_EXECLOG"
exec $REAL_STAT "\$@"
EOF
cat > "$SHIM/mkdir" <<EOF
#!/bin/bash
printf 'mkdir %s\n' "\$*" >> "\$WR_EXECLOG"
exec $REAL_MKDIR "\$@"
EOF
# THE TRIPWIRES. A staleness check that queried the store would be the very failure it
# exists to detect, so the programs that could reach one are present, loud and fatal.
for f in bd dolt git python3 date; do
    cat > "$SHIM/$f" <<'EOF'
#!/bin/bash
printf 'FORBIDDEN %s %s\n' "${0##*/}" "$*" >> "$WR_EXECLOG"
exit 1
EOF
done
chmod +x "$SHIM"/*

EXECLOG="$TMP/execs"; ACT="$TMP/acted"; SHOW="$TMP/show"
: > "$EXECLOG"; : > "$ACT"

# show <unit> <state> <start> [execstart-path] — one block of `systemctl show` output.
show() {
    printf 'Id=%s\nActiveState=%s\nActiveEnterTimestamp=%s\nExecStart={ path=%s ; argv[]=%s %s ; pid=1 }\n\n' \
        "$1" "$2" "$3" "${4:-$CLONE/spira/watchd.sh}" "${4:-$CLONE/spira/watchd.sh}" "exec x" >> "$SHOW"
}
fresh_show() { : > "$SHOW"; show "spira-watch-answers-prod.service" active "@$UNIT_START"; }

# runpass [dry] — one pass, as a FUNCTION, with the exec log truncated immediately before it.
#
# Sourced rather than run because the acceptance criterion is about a PASS, and a whole
# process also pays for conf.sh resolving itself once at startup. Sourcing is also what
# makes the exec count exact: PATH is replaced with the shim directory alone AFTER the
# configuration is loaded, so anything the pass reaches for that is not shimmed fails
# outright instead of being missed.
runpass() {
    : > "$ACT"
    env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$CONF" SPIRA_WATCHERS="${WR_MAN:-$MAN}" \
        WR_EXECLOG="$EXECLOG" WR_ACT="$ACT" WR_SHOW="$SHOW" SHIM="$SHIM" \
        SYSTEMCTL_RC="${SYSTEMCTL_RC:-0}" RESTART_RC="${RESTART_RC:-0}" \
        bash -c '
            . "'"$CLONE"'/spira/watch-refresh.sh"
            : > "$WR_EXECLOG"
            PATH="$SHIM"
            wr_pass "$1"
        ' _ "${1:-}" > "$TMP/out" 2>"$TMP/err"
}
acted()  { tr '\n' ' ' < "$ACT"; }
execs()  { grep -cE '^(systemctl|stat|mkdir|FORBIDDEN)' "$EXECLOG" 2>/dev/null || true; }

echo "the positive control — nothing has changed, and the check looked anyway"
reset_mtimes; fresh_show
runpass; rc=$?
is "the pass exits clean"                    "0" "$rc"
is "and restarts nothing"                    "" "$(acted)"
is "and says nothing"                        "" "$(cat "$TMP/out")"
# WITHOUT THIS the silence above proves nothing: a pass that never ran, or one whose
# systemctl was never reached, restarts nothing in exactly the same way.
has "but it did ask systemd"                 "$(cat "$EXECLOG")" "systemctl --user show"
has "and it did stat the files"              "$(cat "$EXECLOG")" "stat -c %Y %n"
has "one show for every unit at once"        "$(cat "$EXECLOG")" "spira-watch-answers-prod.service"

echo
echo "what one pass costs (law-fence-loops-on-shared-hardware)"
is "a steady pass is exactly two execs"      "2" "$(execs)"
is "one systemctl"  "1" "$(grep -c '^systemctl' "$EXECLOG" || true)"
is "and one stat"   "1" "$(grep -c '^stat' "$EXECLOG" || true)"
# A staleness check that queried the database would BE the failure it exists to detect.
is "and it opens no database, and runs no other program" "" \
   "$(grep '^FORBIDDEN' "$EXECLOG" || true)"
is "nothing it needed was missing from the shims" "" \
   "$(grep -c 'command not found' "$TMP/err" | grep -v '^0$' || true)"

echo
echo "a watcher whose code is newer than its process is restarted"
restarts_on() {          # restarts_on <label> <file to touch>
    reset_mtimes; fresh_show; : > "$RUN/watchd/answers.restarts"
    touch -d "@$NEWER" "$2"
    runpass
    case "$(acted)" in
        *"restart spira-watch-answers-prod.service"*) ;;
        *) bad "$1" "acted: [$(acted)] out: $(cat "$TMP/out")"; return ;;
    esac
    ok "$1"
}
restarts_on "its own target"                 "$COCKPIT/watch-answers.sh"
# THE MEASURED FAILURE WAS A LIBRARY, NOT A TARGET. The watcher's own file was untouched for
# days while the code it sourced was rewritten underneath it.
restarts_on "a library beside its target"    "$COCKPIT/db.sh"
restarts_on "the config file in force"       "$CONF"
restarts_on "conf.sh"                        "$CLONE/spira/conf.sh"
# The manifest is what says which target the row means, so a row repointed is a watcher
# running the wrong program with a perfectly current file behind it.
restarts_on "the manifest"                   "$MAN"
restarts_on "the dispatcher it is started through" "$CLONE/spira/watchd.sh"

# AND THE UNIT'S OWN ExecStart, which is not necessarily this harness's copy. A box carrying
# an install from another checkout runs that tree's dispatcher, and only systemd knows.
reset_mtimes; : > "$SHOW"
show "spira-watch-answers-prod.service" active "@$UNIT_START" "$OTHER/watchd.sh"
touch -d "@$NEWER" "$OTHER/watchd.sh"
runpass
has "the unit's own ExecStart, wherever it points" "$(acted)" "restart spira-watch-answers-prod.service"
has "and the message names the file that caused it" "$(cat "$TMP/out")" "$OTHER/watchd.sh"

echo
echo "a target that carries arguments — the row shape the shipped manifest uses"
# A manifest target is a COMMAND, not only a path: the shipped view row reads
# `@SPIRA_VIEW@ watch`. A pass that stat'ed the target whole would be asking about a file
# named "<program> watch", which exists nowhere — so it would find nothing newer, restart
# nothing, and look exactly like a healthy watcher forever. That is why the discriminating
# assertion here is a RESTART rather than the absence of one.
# THE PROGRAM IS NOT A `*.sh`, deliberately. A watcher's target may be a compiled program,
# and the sibling sweep only covers `*.sh` and `*.py` — so this row's program is reachable
# ONLY through the target, which is what makes the assertions below discriminating. Named
# with an extension, the sweep would catch it whether or not the target was parsed at all.
printf '#!/bin/sh\nsleep 3600\n' > "$COCKPIT/viewer"; chmod +x "$COCKPIT/viewer"
MAN2="$TMP/watchers-argv"
cat > "$MAN2" <<EOF
viewer|daemon|@SPIRA_COCKPIT@/viewer watch|
EOF
argv_show() { : > "$SHOW"; show "spira-watch-viewer-prod.service" active "@$UNIT_START"; }

reset_mtimes; touch -d "@$T0" "$MAN2"; argv_show
WR_MAN="$MAN2" runpass
is "unchanged, it is left alone"             "" "$(acted)"
# The positive control for the line above: an argument in the target must not quietly cost
# the row its whole check.
has "and it was asked about all the same"    "$(cat "$EXECLOG")" "spira-watch-viewer-prod.service"

reset_mtimes; touch -d "@$T0" "$MAN2"; argv_show
touch -d "@$NEWER" "$COCKPIT/viewer"
WR_MAN="$MAN2" runpass
has "the program is taken from the target's first word" "$(acted)" "restart spira-watch-viewer-prod.service"
has "and the message names the program, not the command" "$(cat "$TMP/out")" "$COCKPIT/viewer"
# Asserted at the stat call and not only at the restart: under the whole-string bug the
# program is never among the paths asked about at all, and the `-e` filter then drops the
# unresolvable "<program> watch" silently — so the pass runs, stats a set missing the one
# file that matters, and reports the watcher fresh.
has "the program itself is among the files stat'ed" "$(cat "$EXECLOG")" "$COCKPIT/viewer"

echo
echo "an unchanged watcher is NEVER restarted — the half that regresses silently"
never() {                # never <label> <file to touch, or empty>
    reset_mtimes; fresh_show
    [ -n "${2:-}" ] && touch -d "@$NEWER" "$2"
    runpass
    is "$1" "" "$(acted)"
}
never "nothing touched at all"               ""
# THE RESTART LOOP THIS AVOIDS. A watcher writes state beside itself while it runs; a check
# that stat'ed everything in that directory would restart the watcher on its own output,
# every pass, forever.
never "state the watcher writes while running" "$COCKPIT/state.json"
never "a log beside its target"                "$COCKPIT/scratch.log"
# A file inside the harness that no watcher runs.
never "an unrelated file in the harness"       "$CLONE/spira/watch-refresh.sh"

echo
echo "a log row has no process of ours, and an inactive unit pins nothing"
reset_mtimes; fresh_show; touch -d "@$NEWER" "$COCKPIT/watch-answers.sh"
runpass
hasnt "the log row is never asked about"  "$(cat "$EXECLOG")" "spira-watch@cron"
hasnt "and never restarted"               "$(acted)" "cron"
reset_mtimes; : > "$SHOW"
show "spira-watch-answers-prod.service" inactive ""
touch -d "@$NEWER" "$COCKPIT/watch-answers.sh"
runpass
# Restart=always is what brings a dead unit back; a restart aimed at one here fights
# systemd's own backoff, and there is no process holding stale configuration anyway.
is "an inactive unit is left alone even when stale" "" "$(acted)"

echo
echo "the meter — a restart counter per watcher, which is what would justify hashing"
reset_mtimes; fresh_show; rm -f "$RUN/watchd/answers.restarts"
touch -d "@$NEWER" "$COCKPIT/watch-answers.sh"
runpass
is "the first restart writes 1"  "1" "$(cat "$RUN/watchd/answers.restarts" 2>/dev/null)"
runpass
is "and the second writes 2"     "2" "$(cat "$RUN/watchd/answers.restarts" 2>/dev/null)"
reset_mtimes; fresh_show
runpass
is "a quiet pass leaves it alone" "2" "$(cat "$RUN/watchd/answers.restarts" 2>/dev/null)"
# A COUNTER THAT CLIMBS WITH NOTHING SAYING WHY IS A METER NOBODY CAN ACT ON.
reset_mtimes; fresh_show; touch -d "@$NEWER" "$COCKPIT/db.sh"; runpass
has "and every restart names the file behind it" "$(cat "$TMP/out")" "$COCKPIT/db.sh"
is "a restarting pass costs one exec more, and no more than that" "3" "$(execs)"
# The restart is not bookkeeping: a counter that moved without systemctl being called would
# be a meter measuring itself.
reset_mtimes; fresh_show; touch -d "@$NEWER" "$COCKPIT/db.sh"
n_before="$(cat "$RUN/watchd/answers.restarts" 2>/dev/null)"
RESTART_RC=1 runpass
is "a restart that FAILED is not counted"  "$n_before" "$(cat "$RUN/watchd/answers.restarts" 2>/dev/null)"
has "and it is reported"                   "$(cat "$TMP/err")" "FAILED"
RESTART_RC=0

echo
echo "--dry-run changes nothing"
reset_mtimes; fresh_show; touch -d "@$NEWER" "$COCKPIT/watch-answers.sh"
n_before="$(cat "$RUN/watchd/answers.restarts" 2>/dev/null)"
runpass dry
is "it restarts nothing"        "" "$(acted)"
has "but says what it would do" "$(cat "$TMP/out")" "would restart spira-watch-answers-prod.service"
is "and the meter does not move" "$n_before" "$(cat "$RUN/watchd/answers.restarts" 2>/dev/null)"

echo
echo "a pass that cannot see restarts NOTHING, and says so"
# Each of these is a way for the guard to look healthy while being blind. Silence is the one
# outcome none of them may produce (law-absence-needs-a-positive-control).
reset_mtimes; fresh_show; touch -d "@$NEWER" "$COCKPIT/watch-answers.sh"
SYSTEMCTL_RC=1 runpass; rc=$?
SYSTEMCTL_RC=0
is "a systemctl that will not answer fails the pass" "1" "$rc"
is "and restarts nothing"                            "" "$(acted)"
has "and names it"  "$(cat "$TMP/err")" "systemctl show failed"

# AN OLDER systemd THAT DOES NOT KNOW --timestamp=unix hands back a localised date. Guessing
# at the format is how a check quietly starts answering about nothing.
reset_mtimes; : > "$SHOW"
show "spira-watch-answers-prod.service" active "Mon 2026-09-07 01:35:41 UTC"
touch -d "@$NEWER" "$COCKPIT/watch-answers.sh"
runpass; rc=$?
is "a start time it cannot read is not guessed at" "" "$(acted)"
has "and it says which unit and what it got"       "$(cat "$TMP/err")" "no usable start time"

MANBAD="$TMP/watchers-bad"
printf 'answers|daemon|/bin/true\nbroken row\n' > "$MANBAD"
( MAN="$MANBAD"; reset_mtimes 2>/dev/null; fresh_show
  runpass; rc=$?
  is "a malformed manifest fails the pass"  "1" "$rc"
  is "and restarts nothing"                 "" "$(acted)"
  is "and never even asks systemd"          "" "$(grep '^systemctl' "$EXECLOG" || true)"
  printf '%d %d\n' "$pass" "$fail" > "$TMP/subshell-counts" )
read -r p f < "$TMP/subshell-counts"; pass="$p"; fail="$f"

MANNONE="$TMP/watchers-log-only"
printf 'cron|log|/tmp/x.log\n' > "$MANNONE"
( MAN="$MANNONE"; fresh_show
  runpass; rc=$?
  is "a manifest with no daemon row is not an error" "0" "$rc"
  has "but it is said out loud"  "$(cat "$TMP/err")" "no daemon watchers"
  is "and nothing is asked of systemd"  "" "$(grep '^systemctl' "$EXECLOG" || true)"
  printf '%d %d\n' "$pass" "$fail" > "$TMP/subshell-counts" )
read -r p f < "$TMP/subshell-counts"; pass="$p"; fail="$f"

echo
echo "orphan reaping — watchers outside any spira-watch unit are terminated"
# A fake /proc tree lets us test the reaper without touching real processes or real cgroups.
# PIDs are arbitrary integers; the reaper only reads files, it never signals real pids here
# because wr_sigterm is redefined inside runreap to append to a log instead.
FAKEPROC="$TMP/proc"
REAP_ACT="$TMP/reap_acted"
REAP_OUT="$TMP/reap_out"
REAP_ERR="$TMP/reap_err"

SUPERVISED_PID=10001    # in spira-watch@answers.service cgroup — must never be signalled
PROD_PID=10005          # in spira-watch-answers-prod.service cgroup — equally supervised
ORPHAN_PID=10002        # NOT in any spira-watch unit, running watch-answers.sh
TAIL_PID=10003          # NOT in any spira-watch unit, running watchd.sh tail

mkdir -p "$FAKEPROC/$SUPERVISED_PID" "$FAKEPROC/$PROD_PID" \
         "$FAKEPROC/$ORPHAN_PID" "$FAKEPROC/$TAIL_PID"

# supervised: systemd put it in spira-watch@answers.service (template instance)
printf 'bash\0%s\0' "$COCKPIT/watch-answers.sh" > "$FAKEPROC/$SUPERVISED_PID/cmdline"
printf '0::/user.slice/user-1000.slice/user@1000.service/app.slice/spira-watch@answers.service\n' \
    > "$FAKEPROC/$SUPERVISED_PID/cgroup"

# supervised: non-template unit — spira-watch-answers-prod.service, cgroup has no `@`.
# This is the case the old spira-watch@ guard missed: the cgroup carries the unit name
# literally, so it contains spira-watch but not spira-watch@.
printf 'bash\0%s\0' "$COCKPIT/watch-answers.sh" > "$FAKEPROC/$PROD_PID/cmdline"
printf '0::/user.slice/user-1000.slice/user@1000.service/app.slice/spira-watch-answers-prod.service\n' \
    > "$FAKEPROC/$PROD_PID/cgroup"

# orphan watch-answers.sh: started by hand, outside any spira-watch unit
printf 'bash\0%s\0' "$COCKPIT/watch-answers.sh" > "$FAKEPROC/$ORPHAN_PID/cmdline"
printf '0::/user.slice/user-1000.slice/user@1000.service/\n' \
    > "$FAKEPROC/$ORPHAN_PID/cgroup"

# session tail: watchd.sh tail answers opened by a live session's Monitor — must survive
printf 'bash\0%s\0tail\0answers\0' "$CLONE/spira/watchd.sh" > "$FAKEPROC/$TAIL_PID/cmdline"
printf '0::/user.slice/user-1000.slice/user@1000.service/\n' \
    > "$FAKEPROC/$TAIL_PID/cgroup"

# runreap [dry] — runs wr_reap_orphans against the fake proc tree. wr_sigterm is redefined
# to log the pid it would signal rather than sending a real signal.
runreap() {
    : > "$REAP_ACT"
    env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$CONF" SPIRA_WATCHERS="$MAN" \
        WR_PROC_ROOT="$FAKEPROC" WR_REAP_ACT="$REAP_ACT" \
        bash -c '
            . "'"$CLONE"'/spira/watch-refresh.sh"
            wr_sigterm() { echo "$1" >> "$WR_REAP_ACT"; }
            wr_reap_orphans "$1"
        ' _ "${1:-}" > "$REAP_OUT" 2>"$REAP_ERR"
}
reap_acted() { tr '\n' ' ' < "$REAP_ACT"; }

# THE FOUR CASES: orphaned daemon is gone; template unit, non-template unit, and session tail
# all survive. THE SESSION TAIL IS THE ACCEPTANCE CRITERION FOR sp-fcom: a tail outside
# any spira-watch unit must not be reaped — it is a reader the SessionStart hook just told
# the session to open, and killing it severs that channel (seen failing against code before
# this fix). THE NON-TEMPLATE UNIT IS THE ACCEPTANCE CRITERION FOR sp-2bb8: a standalone
# unit spira-watch-* has no `@` in its cgroup path and was reaped by the old guard, which
# checked only for spira-watch@.
runreap; rc=$?
is "reap pass exits clean"                                    "0" "$rc"
has "the orphan watch-answers.sh is terminated"               "$(reap_acted)" "$ORPHAN_PID"
hasnt "a session tail is NOT terminated (it is a reader)"     "$(reap_acted)" "$TAIL_PID"
hasnt "the template supervised unit is NOT terminated"        "$(reap_acted)" "$SUPERVISED_PID"
hasnt "a non-template supervised unit is NOT terminated"      "$(reap_acted)" "$PROD_PID"

echo
echo "orphan reaping — dry-run says what it would do and sends no signal"
runreap dry; rc=$?
is "--dry-run exits clean"                    "0" "$rc"
is "and sends no signal"                      "" "$(reap_acted)"
has "but says what it would do"               "$(cat "$REAP_OUT")" "would reap orphan pid"
has "naming the orphan pid in the output"     "$(cat "$REAP_OUT")" "$ORPHAN_PID"
hasnt "and not mentioning the supervised pid" "$(cat "$REAP_OUT")" "$SUPERVISED_PID"
hasnt "and not mentioning the non-template supervised pid" "$(cat "$REAP_OUT")" "$PROD_PID"
hasnt "and not mentioning the session tail"   "$(cat "$REAP_OUT")" "$TAIL_PID"

echo
echo "orphan reaping — watchd exec outside any spira-watch unit is a target; tail never is"
# watchd.sh exec is the supervised daemon verb. In practice exec processes ARE in spira-watch
# cgroups (systemd puts them there), so the cgroup guard covers them — the verb check is
# belt-and-braces.
# tail is a reader that a session opens via Monitor; it is never a reap target regardless of
# cgroup (law-bind-the-actor: the reaper must not sever a channel it told the session to open).
mkdir -p "$FAKEPROC/10004"
printf 'bash\0%s\0exec\0answers\0' "$CLONE/spira/watchd.sh" > "$FAKEPROC/10004/cmdline"
printf '0::/user.slice/user-1000.slice/user@1000.service/\n' > "$FAKEPROC/10004/cgroup"
runreap
has "watchd exec outside any spira-watch unit IS a reap target" "$(reap_acted)" "10004"

echo
echo "the entry point, run as systemd runs it"
# Everything above calls the pass as a function. This is the one that proves the file is
# also a program, and that sourcing it — which is how watch-refresh.sh reads the manifest —
# still leaves watchd.sh silent rather than running its own dispatcher.
# WR_PROC_ROOT is set to an empty directory so the orphan reaper (now part of the entry
# point) does not scan the real /proc of the test runner.
EMPTYPROC="$TMP/empty-proc"; mkdir -p "$EMPTYPROC"
reset_mtimes; fresh_show; touch -d "@$NEWER" "$COCKPIT/watch-answers.sh"; : > "$ACT"
out="$(env -i HOME="$TMP/home" PATH="$SHIM:$PATH" SPIRA_CONF="$CONF" SPIRA_WATCHERS="$MAN" \
      SPIRA_PATH="$SHIM" WR_EXECLOG="$EXECLOG" WR_ACT="$ACT" WR_SHOW="$SHOW" \
      WR_PROC_ROOT="$EMPTYPROC" \
      bash "$CLONE/spira/watch-refresh.sh" 2>&1)"; rc=$?
is "it runs"                       "0" "$rc"
has "and restarts the stale unit"  "$(acted)" "restart spira-watch-answers-prod.service"
hasnt "and sourcing watchd.sh printed no manifest of its own" "$out" "|daemon|"
out="$(env -i HOME="$TMP/home" PATH="$SHIM:$PATH" SPIRA_CONF="$CONF" SPIRA_WATCHERS="$MAN" \
      SPIRA_PATH="$SHIM" WR_EXECLOG="$EXECLOG" WR_ACT="$ACT" WR_SHOW="$SHOW" \
      WR_PROC_ROOT="$EMPTYPROC" \
      bash "$CLONE/spira/watch-refresh.sh" --nonsense 2>&1)"; rc=$?
is "an argument it does not know is refused" "2" "$rc"
has "with a usage line"                      "$out" "usage: watch-refresh.sh"

echo
echo "the units — fenced before they are enabled, and no path baked in"
STUB="$TMP/instub"; mkdir -p "$STUB"
cat > "$STUB/systemctl" <<EOF
#!/bin/bash
printf '%s\n' "\$*" >> "$TMP/install.log"
exit 0
EOF
printf '#!/bin/bash\nexit 0\n' > "$STUB/loginctl"
chmod +x "$STUB/systemctl" "$STUB/loginctl"
IHOME="$TMP/ihome"; mkdir -p "$IHOME"
# A separate PROD root, distinct from CLONE (SPIRA_HOME) and RUN (SPIRA_RUN), so the
# assertion can verify that ExecStart resolves from SPIRA_PROD, not from a hardcoded path
# or from SPIRA_HOME. sp-g8ph changed ExecStart from @SPIRA_HOME@ to @SPIRA_PROD@; this
# is the property that change introduced.
# install.sh refuses an unexecutable ExecStart target; all scripts must exist in PROD.
PROD="$TMP/prod-fake"; mkdir -p "$PROD"
cp "$HERE"/*.sh "$PROD/"
: > "$TMP/install.log"
printf 'SPIRA_COCKPIT = %s\nSPIRA_RUN = %s\nSPIRA_WATCHERS = %s\nSPIRA_PATH = %s\nSPIRA_PROD = %s\n' \
    "$COCKPIT" "$RUN" "$MAN" "$STUB" "$PROD" > "$TMP/install.conf"
env -i HOME="$IHOME" PATH="$STUB:$PATH" SPIRA_CONF="$TMP/install.conf" \
    SPIRA_INSTALL_FORCE=1 \
    bash "$CLONE/systemd/install.sh" > "$TMP/install.out" 2>&1
ilog="$(cat "$TMP/install.log")"
has "the stub recorded an install"      "$ilog" "daemon-reload"
has "the refresh timer is enabled"      "$ilog" "enable --now spira-watch-refresh-prod.timer"
# The .service behind a .timer is started BY the timer; enabling it as well runs it once at
# boot, outside the schedule.
hasnt "and the service behind it is not" "$ilog" "enable --now spira-watch-refresh-prod.service"

U="$IHOME/.config/systemd/user/spira-watch-refresh-prod.service"
unit="$(cat "$U" 2>/dev/null)"
has "the service is installed"  "$(ls "$IHOME/.config/systemd/user" 2>/dev/null)" "spira-watch-refresh-prod.service"
# law-fence-loops-on-shared-hardware: anything that polls is fenced BEFORE it is enabled.
has "it is CPU-fenced"          "$unit" "CPUQuota="
has "and it is niced"           "$unit" "Nice="
has "and it cannot hang forever" "$unit" "TimeoutStartSec="
hasnt "no placeholder survives into it" "$unit" "@"
T="$IHOME/.config/systemd/user/spira-watch-refresh-prod.timer"
has "the timer fires every minute" "$(cat "$T" 2>/dev/null)" "OnUnitActiveSec=1min"

# NO PATH IS HARDCODED. Every absolute path in the rendered units must lie under a value
# that came from the config above — which pins all three to non-defaults, so a literal cannot
# pass by coincidence. SPIRA_HOME resolves from $CLONE/spira (conf.sh's own directory),
# SPIRA_PROD from $PROD, and SPIRA_RUN from $RUN.
paths="$(sed 's|file://|file:|' "$U" "$T" 2>/dev/null | grep -oE '[=:]/[^ ]+' | sed 's/^[=:]//')"
is "the path extractor found paths to judge" "yes" "$([ -n "$paths" ] && echo yes || echo no)"
stray=""
while IFS= read -r p; do
    [ -n "$p" ] || continue
    case "$p" in "$CLONE/spira"/*|"$RUN"/*|"$PROD"/*) ;; *) stray="$stray $p" ;; esac
done <<< "$paths"
is "and every one came from configuration" "" "$stray"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
