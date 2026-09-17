#!/usr/bin/env bash
#
# test-watchd-prune.sh — prune removes lock, cursor and pending files for retired watcher names.
#
# THE INCIDENT. sp-xjj39 retired the answers watcher but left its lock file behind in
# $SPIRA_RUN/watchd/. brain-guard.sh's watchers-latched guard iterates *.tail.lock rather than
# reading the manifest, so the stale lock read as a live watcher whose unit was down. Every Bash
# command was blocked with advice to start a unit for a name the manifest no longer knows.
# Unblocked by hand on 2026-09-16 23:58 by renaming the lock file.
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL: without prune, a stale lock file for a retired name persists after a
#    manifest row is removed. This proves the test can detect the problem.
# 2. prune removes lock, cursor and pending files for names not in the manifest.
# 3. prune leaves files for names that are still in the manifest.
# 4. A malformed manifest refuses the prune (does not remove anything).
# 5. prune on an absent watchd dir exits 0 with a message.
#
# No database, no systemd, no real watcher manifest — scratch only.
#
# defect: sp-8zcxp
# covers: spira/watchd.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
WATCHD="$HERE/watchd.sh"

pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s\n        %s\n' "$1" "${2:-}"; }
is()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "want [$2] got [$3]"; }
present() { [ -f "$2" ] && ok "$1" || bad "$1" "file missing: $2"; }
gone()  { [ ! -e "$2" ] && ok "$1" || bad "$1" "file still present: $2"; }

echo "test-watchd-prune.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; WDIR="$RUN/watchd"; mkdir -p "$WDIR"

# One active watcher in the manifest, pinned to a non-default instance.
MAN="$TMP/watchers"
printf 'active-watcher|daemon|/usr/bin/true\n' > "$MAN"

run_prune() {
    env -i \
        PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_INSTANCE=test \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$RUN" \
        SPIRA_WATCHERS="$MAN" \
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

# ---------------------------------------------------------------------------
echo
echo "2. prune removes stale files for the retired name:"

out="$(run_prune)"
rc=$?
is "prune exits 0" "0" "$rc"
gone "retired lock removed"   "$WDIR/retired-watcher.tail.lock"
gone "retired cursor removed" "$WDIR/retired-watcher.cursor"
gone "retired pending removed" "$WDIR/retired-watcher.pending"

# ---------------------------------------------------------------------------
echo
echo "3. prune leaves files for the name still in the manifest:"

present "active lock preserved"   "$WDIR/active-watcher.tail.lock"
present "active cursor preserved" "$WDIR/active-watcher.cursor"
present "active pending preserved" "$WDIR/active-watcher.pending"

# ---------------------------------------------------------------------------
echo
echo "4. malformed manifest refuses the prune:"

# Put new stale files down so we can verify they are NOT removed.
touch "$WDIR/another-retired.tail.lock"
BAD_MAN="$TMP/bad-watchers"
printf 'bad-row-no-pipe\n' > "$BAD_MAN"

out="$(env -i \
    PATH="$PATH" \
    HOME="$TMP/home" \
    SPIRA_INSTANCE=test \
    SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$RUN" \
    SPIRA_WATCHERS="$BAD_MAN" \
    bash "$WATCHD" prune 2>&1)"
rc=$?
is "bad manifest exits non-zero" "1" "$rc"
present "stale lock untouched after bad-manifest prune" "$WDIR/another-retired.tail.lock"
rm -f "$WDIR/another-retired.tail.lock"

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
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
