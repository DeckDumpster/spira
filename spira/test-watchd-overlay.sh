#!/usr/bin/env bash
#
# test-watchd-overlay.sh — an operator overlay manifest joins the harness's own watchers.
#
#   ./test-watchd-overlay.sh
#
# THE GAP THIS CLOSES. watchd.sh already makes a watcher survive a session — a systemd unit,
# a log with a cursor, a re-latch that replays only what was missed — but its manifest was
# spira/watchers, inside the harness repository. An operator's or the Concierge's own watcher
# could not join it without a harness change, so those watchers ran as in-session Monitor
# scripts instead, and died with the session that started them (sp-51eqs).
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL: with no overlay file present, an overlay row is absent from manifest,
#    units and status — proving the later sections would have caught its absence.
# 2. An overlay row in a `*.watchers` file under SPIRA_WATCHERS_OVERLAY joins the manifest,
#    `units` and `status` exactly like a harness row — merged, not a second table.
# 3. `watchd.sh tail <overlay-row>` replays only what the cursor has not yet delivered, the
#    same contract a harness row gets.
# 4. Removing the overlay file and running `watchd.sh prune` retires the row: its cursor and
#    lock files and its installed unit are removed, the same as a retired harness row.
# 5. A malformed overlay file refuses the WHOLE manifest, harness rows included — one bad file
#    is not allowed to look like a partial success.
#
# No database, no real systemd — a stubbed systemctl records what it was asked to disable, and
# a scratch $HOME/.config/systemd/user stands in for the installed unit directory, the same
# fixture test-watchd-prune.sh uses.
#
# tier: T1
# covers: spira/watchd.sh spira/conf.sh spira/watchers UC-operator-channel-35
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
WATCHD="$HERE/watchd.sh"

echo "test-watchd-overlay.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; WDIR="$RUN/watchd"; mkdir -p "$WDIR"

# The harness's own manifest carries one row throughout — every assertion below is about
# whether the OVERLAY row joins it, never about whether the harness's own row survives.
MAN="$TMP/watchers"
printf 'core|daemon|/usr/bin/true\n' > "$MAN"

OVDIR="$TMP/overlay.d"     # not created yet — section 1 runs against a missing directory

# A stubbed systemctl: records every `disable --now <unit>` it was asked to run.
BIN="$TMP/bin"; mkdir -p "$BIN"
SC_LOG="$TMP/systemctl.log"; : > "$SC_LOG"
cat > "$BIN/systemctl" <<MOCK
#!/usr/bin/env bash
case "\$*" in
    *"disable"*"--now"*) printf '%s\n' "\${@: -1}" >> "$SC_LOG" ;;
esac
exit 0
MOCK
chmod +x "$BIN/systemctl"
UNITDIR="$TMP/home/.config/systemd/user"; mkdir -p "$UNITDIR"

# wd <args...> — one watchd.sh call, in a minimal environment: no HOME leaking a real
# operator overlay into this suite, no real systemd, pinned to a non-default instance.
wd() {
    env -i \
        PATH="$BIN:$PATH" \
        HOME="$TMP/home" \
        SPIRA_INSTANCE=test \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$RUN" \
        SPIRA_WATCHERS="$MAN" \
        SPIRA_WATCHERS_OVERLAY="$OVDIR" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        bash "$WATCHD" "$@" 2>/dev/null
}

disabled() { grep -qx "$2" "$SC_LOG" && ok "$1" || bad "$1" "not disabled: $2" "$(cat "$SC_LOG")"; }
gone()     { [ ! -e "$2" ] && ok "$1" || bad "$1" "file still present: $2"; }

# ---------------------------------------------------------------------------
echo
echo "1. POSITIVE CONTROL — no overlay row without an overlay file:"

man1="$(wd manifest)"; rc=$?
is  "manifest exits 0 with no overlay dir" "0" "$rc"
want    "the harness row is there"     "core|daemon|"   "$man1"
nowant  "the overlay row is not there" "sidecar"        "$man1"
nowant  "no overlay row in units"      "sidecar"        "$(wd units)"
nowant  "no overlay row in status"     "sidecar"        "$(wd status)"

# ---------------------------------------------------------------------------
echo
echo "2. an overlay row joins the manifest, units and status:"

mkdir -p "$OVDIR"
printf 'sidecar|daemon|/usr/bin/true\n' > "$OVDIR/ops.watchers"

man2="$(wd manifest)"; rc=$?
is  "manifest still exits 0"                 "0" "$rc"
want "the harness row is still there"        "core|daemon|"    "$man2"
want "the overlay row joined it"             "sidecar|daemon|" "$man2"

units2="$(wd units)"
want "the overlay daemon has a unit name"    "sidecar" "$units2"

status2="$(wd status)"
want "status lists the overlay row"          "sidecar" "$status2"

# ---------------------------------------------------------------------------
echo
echo "3. tail replays only what the cursor has not yet delivered:"

LOG="$WDIR/sidecar.log"
printf 'line one\nline two\n' > "$LOG"
# drain --all marks both lines read and advances the cursor to 2 — no reliance on
# SPIRA_ACTIONABLE matching this suite's own text.
wd drain sidecar --all >/dev/null
is  "the cursor is at the end of the backlog" "2" "$(cat "$WDIR/sidecar.cursor" 2>/dev/null)"

printf 'line three\n' >> "$LOG"
out="$TMP/tail.out"; err="$TMP/tail.err"
( env -i PATH="$BIN:$PATH" HOME="$TMP/home" SPIRA_INSTANCE=test SPIRA_CONF=/nonexistent \
      SPIRA_RUN="$RUN" SPIRA_WATCHERS="$MAN" SPIRA_WATCHERS_OVERLAY="$OVDIR" \
      bash "$WATCHD" tail sidecar --all >"$out" 2>"$err" ) &
tp=$!

poll_for() {
    local timeout="$1"; shift
    local iters=$(( timeout * 5 )) i=0
    while [ "$i" -lt "$iters" ]; do
        "$@" && return 0
        sleep 0.2; i=$((i+1))
    done
    return 1
}
_has_line_three() { grep -q "line three" "$out" 2>/dev/null; }
poll_for 5 _has_line_three
want    "the new line was delivered"        "line three" "$(cat "$out" 2>/dev/null)"
nowant  "the already-read backlog was not replayed" "line one" "$(cat "$out" 2>/dev/null)"

kill -TERM "$tp" 2>/dev/null
wait "$tp" 2>/dev/null

# ---------------------------------------------------------------------------
echo
echo "4. removing the overlay file and pruning retires the row:"

touch "$UNITDIR/spira-watch-sidecar-test.service"
rm -f "$OVDIR/ops.watchers"

pout="$(wd prune)"; prc=$?
is  "prune exits 0"                          "0" "$prc"
gone "the overlay watcher's cursor is gone"  "$WDIR/sidecar.cursor"
gone "the overlay watcher's unit is gone"    "$UNITDIR/spira-watch-sidecar-test.service"
disabled "the overlay watcher's unit was disabled" "spira-watch-sidecar-test.service"

man4="$(wd manifest)"
nowant "the retired row is gone from the manifest" "sidecar" "$man4"
want   "the harness row survived the prune"        "core|daemon|" "$man4"

# ---------------------------------------------------------------------------
echo
echo "5. a malformed overlay file refuses the whole manifest:"

mkdir -p "$OVDIR"
printf 'bad-row-no-pipe\n' > "$OVDIR/broken.watchers"

out5="$(wd manifest)"; rc5=$?
is  "a malformed overlay file exits non-zero" "1" "$rc5"
is  "and prints nothing"                      "" "$out5"
rm -f "$OVDIR/broken.watchers"

# ---------------------------------------------------------------------------
echo
tl_summary
