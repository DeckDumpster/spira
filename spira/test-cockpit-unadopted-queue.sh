#!/usr/bin/env bash
#
# test-cockpit-unadopted-queue.sh — the unadopted-refs probe must not count harness-owned
# namespaces (queue/*, suite-state/*) as orphans, even when their tip is already on base.
#
# THE SCAR. An aeon following sop-unadopted-refs-stale-db deleted spira/queue/<stamp> while
# its CI was running. The probe counted the branch as SP_UNADOPTED (no bead resolves for a
# queue suffix), the watchtower filed an incident, and the SOP said to delete it. The local
# deletion did no damage only because origin kept the ref. (sp-ctag9)
#
# THE FIX. The reapable set is defined positively: only spira/sp-* refs (bead-id shaped) are
# candidates for the unadopted check. Harness-owned namespaces skip the check entirely and
# are counted in SP_PROTECTED. The open batch branch in $SPIRA_QUEUE_DIR/<repo>/open is also
# protected regardless of its name.
#
# defect: sp-ctag9
# covers: spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require cockpit-unadopted-queue
testdb_up cockpit-unadopted-queue || exit 1

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
BASE_PATH="$PATH"
BD_PATH="${SPIRA_PATH:-}"
REAL_BD="$(command -v "${SPIRA_BD:-bd}" 2>/dev/null)"
[ -n "$REAL_BD" ] || { echo "SKIP cockpit-unadopted-queue: no bd binary" >&2; exit 77; }

REPO="$TMP/repo"
git init -q "$REPO"
git -C "$REPO" commit --allow-empty -m "init" -q

MAP="$TMP/repo-map"
cat > "$MAP" <<MAP
# name | path | land | base | format | gate
repo | $REPO | push | | |
MAP
RUN="$TMP/run"; mkdir -p "$RUN"

# No beads needed: the probe classifies refs by name, then consults the db.
QSTAMP="20260917T173304Z"
SUITE_BR="test-reclaim-slay-branch-guard-20260917T164605Z"

# spira/queue/<stamp> — a live batch PR branch. Its tip is already on main (same commit),
# so the old probe would count it as SP_UNADOPTED. After the fix it must be SP_PROTECTED.
git -C "$REPO" branch "spira/queue/$QSTAMP"

# spira/suite-state/<name> — written by the quarantine path. Same shape: tip on main.
git -C "$REPO" branch "spira/suite-state/$SUITE_BR"

# spira/sp-true-stray — a true bead-shaped ref with no bead and tip on main.
# Positive control: the probe MUST count this as SP_UNADOPTED=1.
git -C "$REPO" branch "spira/sp-true-stray"

# Open batch record for "repo" naming the queue branch.
QDIR="$RUN/queue/repo"
mkdir -p "$QDIR"
{
    printf 'pr=56\n'
    printf 'head=abc\n'
    printf 'base=def\n'
    printf 'members=sp-foo:abc\n'
    printf 'opened=1726598000\n'
    printf 'branch=spira/queue/%s\n' "$QSTAMP"
} > "$QDIR/open"

out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO=repo \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$REAL_BD" \
    SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_PATH="$BD_PATH" \
    SPIRA_QUEUE_DIR="$RUN/queue" \
    bash "$HERE/cockpit.sh" once 2>/dev/null)"

val() { printf '%s' "$out" | grep "^$1=" | head -1 | sed "s/^$1=//; s/^'//; s/'$//"; }

echo "harness-owned namespaces are not counted as unadopted:"

# THE POSITIVE CONTROL: the true stray is counted (probe can find something).
is "SP_UNADOPTED counts sp-true-stray" "1" "$(val SP_UNADOPTED)"
want "SP_UNADOPTED_NAMES names the true stray" "sp-true-stray" "$(val SP_UNADOPTED_NAMES)"

# THE GUARD: queue and suite-state branches are not counted as orphans.
nowant "SP_UNADOPTED_NAMES excludes queue branch"       "queue/"       "$(val SP_UNADOPTED_NAMES)"
nowant "SP_UNADOPTED_NAMES excludes suite-state branch" "suite-state/" "$(val SP_UNADOPTED_NAMES)"

# THE ACCOUNTING: protected refs are named.
want "SP_PROTECTED_NAMES names the queue branch"       "queue/$QSTAMP"  "$(val SP_PROTECTED_NAMES)"
want "SP_PROTECTED_NAMES names the suite-state branch" "suite-state/$SUITE_BR" "$(val SP_PROTECTED_NAMES)"

echo
printf 'test-cockpit-unadopted-queue: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
