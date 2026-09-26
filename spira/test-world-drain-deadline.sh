#!/usr/bin/env bash
#
# test-world-drain-deadline.sh — drain --deadline slays live aeons at the deadline.
#
# WHAT THIS COVERS
# ----------------
# drain --deadline N: on reaching N seconds with aeons still live, slay them
# (--keep-work --reopen) rather than warning and leaving them running. The operator
# who gave a deadline cannot proceed with aeons alive; warn-and-return at a named
# deadline is not useful.
#
# THREE PROPERTIES (law-absence-needs-a-positive-control):
#
#   1. POSITIVE CONTROL — no live aeons: --deadline 0 exits 0 and says DRAINED without
#      calling slay.sh. A stub that always fails would look green without this.
#
#   2. --deadline 0 with a live aeon: slay.sh is called with --bead <id>, --keep-work
#      and --reopen; drain exits 0; the stamp stays (fleet quiet, gate still held).
#
#   3. --timeout 0 with a live aeon: NOT DRAINED, exit 1, slay.sh not called (unchanged).
#
# defect: sp-yqslx
# covers: spira/world.sh
set -uo pipefail
set -m
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-world-drain-deadline.sh"

TMP="$(mktemp -d)"
WORKER_PID=""
trap 'kill -- -"$WORKER_PID" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM

SH="$TMP/spira"
RUN="$TMP/run"
SLAY_CALLS="$TMP/slay-calls"
export SLAY_CALLS
mkdir -p "$SH" "$RUN"

cp "$HERE/world.sh" "$HERE/conf.sh" "$SH/"

# Stub systemctl: answers inactive for everything so halt/drain banner checks stay quiet.
printf '#!/usr/bin/env bash\necho inactive\nexit 3\n' > "$TMP/systemctl"
chmod +x "$TMP/systemctl"

# Recording slay.sh: appends full argument list to SLAY_CALLS on each call, exits 0.
cat > "$SH/slay.sh" <<'SLAY'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SLAY_CALLS"
exit 0
SLAY
chmod +x "$SH/slay.sh"

# Fake aeon.sh so live_aeons() finds the process in /proc via argv match.
printf '#!/usr/bin/env bash\nsleep 120\n' > "$SH/aeon.sh"; chmod +x "$SH/aeon.sh"

drain() {
    rc=0
    out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_CONF="$TMP/no.conf" \
           SPIRA_DB="$TMP/no-db" SPIRA_SYSTEMCTL="$TMP/systemctl" \
           bash "$SH/world.sh" drain "$@" 2>&1)" || rc=$?
}

TEST_BEAD="sp-drain-test"

# --------------------------------------------------------------------------------------
# 1. POSITIVE CONTROL — no live aeons.
# --------------------------------------------------------------------------------------
echo
echo "positive control — no live aeons:"

: > "$SLAY_CALLS"; rm -f "$RUN/world.draining"
drain --deadline 0
[ "$rc" = "0" ] && ok "--deadline 0 with no aeons exits 0" \
               || bad "--deadline 0 with no aeons exits 0" "got rc=$rc"
want "says DRAINED" "DRAINED" "$out"
slay_out="$(cat "$SLAY_CALLS" 2>/dev/null)"
nowant "does not call slay.sh when nobody is running" "--bead" "$slay_out"

rm -f "$RUN/world.draining"

# --------------------------------------------------------------------------------------
# 2. --deadline 0 WITH A LIVE AEON.
# --------------------------------------------------------------------------------------
echo
echo "drain --deadline 0 with a live aeon:"

bash "$SH/aeon.sh" & WORKER_PID=$!
sleep 0.3   # let the process appear in /proc
printf '%s\n' "$WORKER_PID" > "$RUN/aeon-valefor-${TEST_BEAD}.pid"

: > "$SLAY_CALLS"; rm -f "$RUN/world.draining"
drain --deadline 0
slay_args="$(cat "$SLAY_CALLS" 2>/dev/null)"

[ "$rc" = "0" ] && ok "--deadline exits 0 after slaying" \
               || bad "--deadline exits 0 after slaying" "got rc=$rc"
want "slay.sh was called"                       "--bead"      "$slay_args"
want "slay.sh received the correct bead id"     "$TEST_BEAD"  "$slay_args"
want "slay.sh received --keep-work"             "--keep-work" "$slay_args"
want "slay.sh received --reopen"                "--reopen"    "$slay_args"
nowant "slay.sh did not receive --close"        "--close"     "$slay_args"
want   "output names the slain aeon"            "$TEST_BEAD"  "$out"
nowant "does not print NOT DRAINED"             "NOT DRAINED" "$out"
[ -f "$RUN/world.draining" ] && ok "stamp stays (gate held after slay)" \
                             || bad "stamp stays" "stamp was removed"

rm -f "$RUN/aeon-valefor-${TEST_BEAD}.pid"
kill -- -"$WORKER_PID" 2>/dev/null; wait "$WORKER_PID" 2>/dev/null; WORKER_PID=""
rm -f "$RUN/world.draining"

# --------------------------------------------------------------------------------------
# 3. --timeout 0 WITH A LIVE AEON — unchanged warn-and-return behaviour.
# --------------------------------------------------------------------------------------
echo
echo "drain --timeout 0 with a live aeon (warn-and-return unchanged):"

bash "$SH/aeon.sh" & WORKER_PID=$!
sleep 0.3
printf '%s\n' "$WORKER_PID" > "$RUN/aeon-valefor-${TEST_BEAD}.pid"

: > "$SLAY_CALLS"; rm -f "$RUN/world.draining"
drain --timeout 0
slay_args="$(cat "$SLAY_CALLS" 2>/dev/null)"

[ "$rc" = "1" ] && ok "--timeout exits 1 when aeon is live" \
               || bad "--timeout exits 1 when aeon is live" "got rc=$rc"
want   "prints NOT DRAINED"                    "NOT DRAINED" "$out"
want   "says summons remain gated"             "REMAIN GATED" "$out"
nowant "--timeout does not call slay.sh"       "--bead"       "${slay_args:-}"

rm -f "$RUN/aeon-valefor-${TEST_BEAD}.pid"
kill -- -"$WORKER_PID" 2>/dev/null; wait "$WORKER_PID" 2>/dev/null; WORKER_PID=""
rm -f "$RUN/world.draining"

# --------------------------------------------------------------------------------------
# 4. GAP G11 (docs/test-plan/instance-lifecycle.md) — world.sh start revives an inactive
#    watcher service, and leaves a oneshot or already-active one alone. Previously only
#    source-grepped (test-world-start-revives-watchers.sh); this drives world.sh start for
#    real against a per-unit recording systemctl stub — the same fixture shape this file
#    already uses for slay.sh above, extended to answer per unit instead of uniformly.
# --------------------------------------------------------------------------------------
echo
echo "gap G11 — start revives an inactive watcher, spares oneshot/active ones:"

G11_CALLS="$TMP/g11-calls"; export G11_CALLS
cat > "$TMP/systemctl-g11" <<'SC'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$G11_CALLS"
verb=""; subj=""
for a; do
    case "$a" in --user|--no-legend|--all|-p|--value|--state=active) continue ;; esac
    if [ -z "$verb" ]; then verb="$a"
    elif [ -z "$subj" ]; then subj="$a"
    fi
done
case "$verb" in
    list-unit-files)
        case "$*" in
            *'spira-watch@*'*) exit 0 ;;
            *'spira-watch-'*) printf 'spira-watch-inactive-prod.service enabled\nspira-watch-oneshot-prod.service enabled\nspira-watch-active-prod.service enabled\n' ;;
            *) exit 0 ;;
        esac
        exit 0 ;;
    list-units) exit 0 ;;
    show)
        [ "$subj" = "spira-watch-oneshot-prod.service" ] && echo oneshot || echo simple
        exit 0 ;;
    is-active)
        [ "$subj" = "spira-watch-active-prod.service" ] && { echo active; exit 0; }
        echo inactive; exit 3 ;;
    is-enabled) echo disabled; exit 1 ;;
    start) exit 0 ;;
    *) exit 0 ;;
esac
SC
chmod +x "$TMP/systemctl-g11"

: > "$G11_CALLS"
g11_out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_CONF="$TMP/no.conf" SPIRA_DB="$TMP/no-db" \
           SPIRA_SYSTEMCTL="$TMP/systemctl-g11" SPIRA_INSTANCE=prod \
           bash "$SH/world.sh" start 2>&1)"
g11_calls="$(cat "$G11_CALLS")"

want   "an inactive watcher is started"             "start spira-watch-inactive-prod.service" "$g11_calls"
nowant "a oneshot watcher is not started directly"  "start spira-watch-oneshot-prod.service"  "$g11_calls"
nowant "an already-active watcher is not restarted" "start spira-watch-active-prod.service"   "$g11_calls"
want   "world.sh reports the revived watcher"       "started spira-watch-inactive-prod.service" "$g11_out"

echo
tl_summary
