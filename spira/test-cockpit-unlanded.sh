#!/usr/bin/env bash
#
# test-cockpit-unlanded.sh — closed beads with a branch not yet on the base.
#
#   ./test-cockpit-unlanded.sh
#
# THE FAILURE THIS SUITE EXISTS FOR. Between "an aeon closed it" and "a commit on the base
# branch names it" there is a queue that was previously invisible on the health pane. The
# collector now emits SP_PEND rows for this population: closed beads that have a branch
# which has not yet been merged.
#
# FOUR BEADS, THREE SHAPES:
#   sp-aaa  closed, branch exists, NOT on base       → appears in PEND (awaiting)
#   sp-bbb  closed, commit on base                   → does NOT appear (landed)
#   sp-ccc  closed, NO branch, NOT on base           → does NOT appear (never landed)
#   sp-ddd  closed, branch exists in master-based repo, NOT on base → appears, tests master
#
# defect: sp-a5ga
# covers: spira/cockpit.sh cockpit/health.sh
# scar: closed beads with a branch not yet on the base were invisible on the health pane; the queue between "aeon closed it" and "commit on the base branch" was missing.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require cockpit-unlanded
testdb_up cockpit-unlanded || exit 1

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# Git identity for fixture commits — required in the container (no ~/.gitconfig).
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
BASE_PATH="$PATH"
BD_PATH="${SPIRA_PATH:-}"
REAL_BD="$(PATH="$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" command -v bd)"
[ -n "$REAL_BD" ] || { echo "SKIP cockpit-unlanded: no bd binary" >&2; exit 77; }
# After testdb_up, PATH has TESTDB_BIN prepended; command -v bd returns the full absolute
# path to the embedded binary symlink there (TESTDB_BIN/bd). Using the production binary
# (CGO_ENABLED=0, the REAL_BD) fails the conf.sh migrate schema check on embedded stores.
TESTDB_BD_PATH="$(command -v bd)"

ALPHA="$TMP/alpha"; BETA="$TMP/beta"

# Alpha: main-based. sp-bbb's work is on main; sp-aaa has a branch not on main; sp-ccc has
# no branch at all.
git init -q -b main "$ALPHA"
git -C "$ALPHA" commit --allow-empty -m "init" -q
git -C "$ALPHA" commit --allow-empty -m "sp-bbb landed work" -q
git -C "$ALPHA" checkout -q -b spira/sp-aaa
git -C "$ALPHA" commit --allow-empty -m "sp-aaa work" -q
git -C "$ALPHA" checkout -q main

# Beta: master-based. sp-ddd has a branch not on master.
git init -q -b master "$BETA"
git -C "$BETA" commit --allow-empty -m "init" -q
git -C "$BETA" checkout -q -b spira/sp-ddd
git -C "$BETA" commit --allow-empty -m "sp-ddd work" -q
git -C "$BETA" checkout -q master

MAP="$TMP/repo-map"
cat > "$MAP" <<MAP
# name | path | land | base | format | gate
alpha | $ALPHA | push | main | |
beta  | $BETA  | push | master | |
MAP

RUN="$TMP/run"; mkdir -p "$RUN"

# .log files mark that an aeon worked the bead.
for b in sp-aaa sp-bbb sp-ccc sp-ddd; do
    printf '{"type":"system","subtype":"init"}\n' > "$RUN/$b.log"
done

# Closed times: sp-ddd oldest, then sp-ccc, sp-bbb, sp-aaa newest.
AGO20="$(date -u -d '20 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-20M +%Y-%m-%dT%H:%M:%SZ)"
AGO15="$(date -u -d '15 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-15M +%Y-%m-%dT%H:%M:%SZ)"
AGO10="$(date -u -d '10 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-10M +%Y-%m-%dT%H:%M:%SZ)"
AGO5="$(date -u -d '5 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-5M +%Y-%m-%dT%H:%M:%SZ)"

testdb_seed <<JSONL
{"id":"sp-aaa","title":"work not landed","status":"closed","priority":1,"closed_at":"$AGO5","labels":["spira","plan","repo:alpha"]}
{"id":"sp-bbb","title":"work already landed","status":"closed","priority":2,"closed_at":"$AGO10","labels":["spira","plan","repo:alpha"]}
{"id":"sp-ccc","title":"work no branch","status":"closed","priority":1,"closed_at":"$AGO15","labels":["spira","plan","repo:alpha"]}
{"id":"sp-ddd","title":"work in master repo","status":"closed","priority":0,"closed_at":"$AGO20","labels":["spira","plan","repo:beta"]}
JSONL

out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$ALPHA" SPIRA_HOME_REPO=alpha \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_PATH="$BD_PATH" \
    bash "$HERE/cockpit.sh" once 2>/dev/null)"

val() { printf '%s' "$out" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

echo "unlanded section — four beads, three shapes:"

# ======================================================================================
# THE COUNTS — three-way split: landed, awaiting (has branch), never (no branch)
is "SP_CLOSED counts all four" "4" "$(val SP_CLOSED)"
is "SP_LANDED counts only sp-bbb" "1" "$(val SP_LANDED)"
is "SP_AWAITING_LAND counts two (aaa, ddd have branches)" "2" "$(val SP_AWAITING_LAND)"
is "SP_UNLANDED counts one (ccc has no branch)" "1" "$(val SP_UNLANDED)"

# ======================================================================================
# THE PEND SECTION — individual awaiting beads
is "SP_PEND_N is 2 (sp-aaa and sp-ddd)" "2" "$(val SP_PEND_N)"

# sp-ddd is older (closed 20m ago) so it comes first
want "SP_PEND0 contains sp-ddd (oldest)" "sp-ddd" "$(val SP_PEND0)"
want "SP_PEND1 contains sp-aaa (newer)" "sp-aaa" "$(val SP_PEND1)"

# sp-bbb must NOT appear (it already landed)
nowant "SP_PEND0 does not contain sp-bbb" "sp-bbb" "$(val SP_PEND0)"
nowant "SP_PEND1 does not contain sp-bbb" "sp-bbb" "$(val SP_PEND1)"

# sp-ccc must NOT appear (no branch — never-landed, not awaiting)
nowant "SP_PEND0 does not contain sp-ccc" "sp-ccc" "$(val SP_PEND0)"
nowant "SP_PEND1 does not contain sp-ccc" "sp-ccc" "$(val SP_PEND1)"

# Priorities in the rows
want "SP_PEND0 has P0 for sp-ddd" "P0" "$(val SP_PEND0)"
want "SP_PEND1 has P1 for sp-aaa" "P1" "$(val SP_PEND1)"

# SP_PEND_OLDEST should be a non-zero time string
[ "$(val SP_PEND_OLDEST)" != "0" ] && [ "$(val SP_PEND_OLDEST)" != "?" ] \
    && ok "SP_PEND_OLDEST is a non-zero age" \
    || bad "SP_PEND_OLDEST is a non-zero age" "got [$(val SP_PEND_OLDEST)]"

# Titles
want "SP_PEND0 carries the title" "work in master repo" "$(val SP_PEND0)"
want "SP_PEND1 carries the title" "work not landed" "$(val SP_PEND1)"

# ======================================================================================
# THE PANE — does health.sh render it?
PANE="$HERE/../cockpit/health.sh"
# Write a minimal snapshot from the probe output so the pane can source it.
{
    printf '%s\n' "$out" | python3 -c '
import sys
for line in sys.stdin:
    line = line.rstrip("\n")
    if "=" not in line: continue
    k, _, v = line.partition("=")
    k = k.strip()
    if not k or not (k[0].isalpha() or k[0] == "_"): continue
    print("%s=%s" % (k, "\x27" + v.replace("\x27", "\x27\\\x27\x27") + "\x27"))
'
} > "$RUN/cockpit.env"

pane="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$ALPHA" \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_FAYTHS=t \
    bash "$PANE" once 0 120 2>/dev/null)"

want "pane renders UNLND header" "UNLND" "$pane"
want "pane renders sp-ddd" "sp-ddd" "$pane"
want "pane renders sp-aaa" "sp-aaa" "$pane"
nowant "pane does not render sp-bbb" "sp-bbb" "$(printf '%s' "$pane" | grep -v 'landed\|BEADS\|RECENT')"

# ======================================================================================
# THE ZERO CASE — when nothing is pending, the section says so in words.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-eee","title":"already landed","status":"closed","priority":1,"closed_at":"$AGO5","labels":["spira","plan","repo:alpha"]}
JSONL
printf '{"type":"system","subtype":"init"}\n' > "$RUN/sp-eee.log"
# sp-eee is closed and its commit message is on main (we baked it). No branch.
git -C "$ALPHA" commit --allow-empty -m "sp-eee landed" -q

out_zero="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$ALPHA" SPIRA_HOME_REPO=alpha \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_PATH="$BD_PATH" \
    bash "$HERE/cockpit.sh" once 2>/dev/null)"
val_z() { printf '%s' "$out_zero" | grep "^$1=" | head -1 | sed "s/^$1=//"; }
is "SP_PEND_N is 0 when no beads are pending" "0" "$(val_z SP_PEND_N)"

# ======================================================================================
# POSITIVE CONTROL
want "output contains SP_AT" "SP_AT=" "$out"
want "output contains SP_PEND_N" "SP_PEND_N=" "$out"

echo
printf 'test-cockpit-unlanded: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
