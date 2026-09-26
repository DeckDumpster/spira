#!/usr/bin/env bash
#
# test-watchd-prune.sh — prune removes lock, cursor and pending files, and the installed
# unit, for retired watcher names.
#
# THE INCIDENT. sp-xjj39 retired the answers watcher but left its lock file behind in
# $SPIRA_RUN/watchd/. brain-guard.sh's watchers-latched guard iterates *.tail.lock rather than
# reading the manifest, so the stale lock read as a live watcher whose unit was down. Every Bash
# command was blocked with advice to start a unit for a name the manifest no longer knows.
# Unblocked by hand on 2026-09-16 23:58 by renaming the lock file.
#
# sp-07yxy widened the same trap to units: a retired daemon row left its systemd unit enabled,
# and it ran on into the failed state with nothing to disable it.
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL: without prune, a stale lock file and an orphan unit for a retired name
#    both persist after a manifest row is removed. This proves the test can detect the problem.
# 2. prune removes lock, cursor and pending files, and disables+removes the unit, for names
#    not in the manifest as a daemon row.
# 3. prune leaves files and the unit alone for names still in the manifest.
# 4. A malformed manifest refuses the prune (does not remove anything, including no unit).
# 5. prune on an absent watchd dir exits 0 with a message.
#
# No database, no real systemd — a stubbed systemctl records what it was asked to disable, and
# a scratch $HOME/.config/systemd/user stands in for the installed unit directory.
#
# defect: sp-8zcxp
# tier: T1
# covers: spira/watchd.sh UC-operator-channel-35
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
WATCHD="$HERE/watchd.sh"

present() { [ -f "$2" ] && ok "$1" || bad "$1" "file missing: $2"; }
gone()  { [ ! -e "$2" ] && ok "$1" || bad "$1" "file still present: $2"; }

echo "test-watchd-prune.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; WDIR="$RUN/watchd"; mkdir -p "$WDIR"

# One active watcher in the manifest, pinned to a non-default instance.
MAN="$TMP/watchers"
printf 'active-watcher|daemon|/usr/bin/true\n' > "$MAN"

# A stubbed systemctl: records every `disable --now <unit>` it was asked to run, and
# otherwise answers empty — the real membership evidence comes from the DEST scan below,
# which is also how install.sh's own prune catches an orphan systemd has not indexed yet.
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

# The installed-unit directory watchd.sh prunes from: cmd_prune reads $HOME/.config/systemd/user.
UNITDIR="$TMP/home/.config/systemd/user"; mkdir -p "$UNITDIR"
touch "$UNITDIR/spira-watch-active-watcher-test.service"
touch "$UNITDIR/spira-watch-retired-watcher-test.service"

disabled() { grep -qx "$2" "$SC_LOG" && ok "$1" || bad "$1" "not disabled: $2" "$(cat "$SC_LOG")"; }
not_disabled() { grep -qx "$2" "$SC_LOG" && bad "$1" "wrongly disabled: $2" || ok "$1"; }

run_prune() {
    env -i \
        PATH="$BIN:$PATH" \
        HOME="$TMP/home" \
        SPIRA_INSTANCE=test \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$RUN" \
        SPIRA_WATCHERS="$MAN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        bash "$WATCHD" prune 2>&1
}

# ---------------------------------------------------------------------------
echo
echo "1. POSITIVE CONTROL — stale files persist without prune:"

# Create lock/cursor/pending for a retired watcher not in the manifest.
touch "$WDIR/retired-watcher.tail.lock"
touch "$WDIR/retired-watcher.cursor"
touch "$WDIR/retired-watcher.pending"

# And files for the active watcher.
touch "$WDIR/active-watcher.tail.lock"
printf '42\n' > "$WDIR/active-watcher.cursor"
touch "$WDIR/active-watcher.pending"

present "retired lock exists before prune"   "$WDIR/retired-watcher.tail.lock"
present "retired cursor exists before prune" "$WDIR/retired-watcher.cursor"
present "retired pending exists before prune" "$WDIR/retired-watcher.pending"
present "retired unit exists before prune" "$UNITDIR/spira-watch-retired-watcher-test.service"
not_disabled "retired unit not yet disabled" "spira-watch-retired-watcher-test.service"

# ---------------------------------------------------------------------------
echo
echo "2. prune removes stale files for the retired name:"

out="$(run_prune)"
rc=$?
is "prune exits 0" "0" "$rc"
gone "retired lock removed"   "$WDIR/retired-watcher.tail.lock"
gone "retired cursor removed" "$WDIR/retired-watcher.cursor"
gone "retired pending removed" "$WDIR/retired-watcher.pending"
gone "retired unit file removed" "$UNITDIR/spira-watch-retired-watcher-test.service"
disabled "retired unit disabled" "spira-watch-retired-watcher-test.service"

# ---------------------------------------------------------------------------
echo
echo "3. prune leaves files for the name still in the manifest:"

present "active lock preserved"   "$WDIR/active-watcher.tail.lock"
present "active cursor preserved" "$WDIR/active-watcher.cursor"
present "active pending preserved" "$WDIR/active-watcher.pending"
present "active unit file preserved" "$UNITDIR/spira-watch-active-watcher-test.service"
not_disabled "active unit never disabled" "spira-watch-active-watcher-test.service"

# ---------------------------------------------------------------------------
echo
echo "4. malformed manifest refuses the prune:"

# Put new stale files, and a new orphan unit, down so we can verify neither is touched.
touch "$WDIR/another-retired.tail.lock"
touch "$UNITDIR/spira-watch-another-retired-test.service"
BAD_MAN="$TMP/bad-watchers"
printf 'bad-row-no-pipe\n' > "$BAD_MAN"

out="$(env -i \
    PATH="$BIN:$PATH" \
    HOME="$TMP/home" \
    SPIRA_INSTANCE=test \
    SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$RUN" \
    SPIRA_WATCHERS="$BAD_MAN" \
    SPIRA_SYSTEMCTL="$BIN/systemctl" \
    bash "$WATCHD" prune 2>&1)"
rc=$?
is "bad manifest exits non-zero" "1" "$rc"
present "stale lock untouched after bad-manifest prune" "$WDIR/another-retired.tail.lock"
present "orphan unit untouched after bad-manifest prune" "$UNITDIR/spira-watch-another-retired-test.service"
not_disabled "orphan unit not disabled by bad-manifest prune" "spira-watch-another-retired-test.service"
rm -f "$WDIR/another-retired.tail.lock" "$UNITDIR/spira-watch-another-retired-test.service"

# ---------------------------------------------------------------------------
echo
echo "5. prune on absent watchd dir exits 0:"

RUN2="$TMP/no-such-run"
out="$(env -i \
    PATH="$PATH" \
    HOME="$TMP/home" \
    SPIRA_INSTANCE=test \
    SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$RUN2" \
    SPIRA_WATCHERS="$MAN" \
    bash "$WATCHD" prune 2>&1)"
rc=$?
is "absent dir exits 0" "0" "$rc"

# ---------------------------------------------------------------------------
echo
tl_summary
