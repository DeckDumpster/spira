#!/usr/bin/env bash
#
# test-cockpit-accept.sh — the ACCEPTANCE row renders the stale/skip states, not
# only the green one.
#
#   ./test-cockpit-accept.sh
#
# Covers the ACCEPTANCE section of unsent_keys (spira/cockpit.sh) and its render
# in health.sh's GATE block:
#   - a repo with no acceptance note at all renders NEVER, not FAIL and not 0
#   - a stale FAIL note renders FAIL plus the count of releases cut since
#   - an unreadable SPIRA_PROD renders ? for every field, never 0
#   - a fresh PASS with nothing cut since renders SP_ACCEPT_SINCE=0
#   - health.sh's rendered row keeps NEVER, FAIL and ? visually distinct
#
# defect: sp-hhggy
# covers: spira/cockpit.sh cockpit/health.sh
# scar: acceptance had been discarding its verdict for days while the cockpit
#   showed nothing, because a verdict-only row cannot distinguish stale calm
#   from an untested release (law-absence-needs-a-positive-control).
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"

run_unsent() {
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        "$@" \
        bash "$HERE/cockpit.sh" unsent 2>/dev/null
}

# A non-default SPIRA_PROD: the code strips a trailing "/spira" to find the repo
# root, so the fixture's prod checkout lives at $TMP/prodhouse and SPIRA_PROD
# names its (nonexistent) spira subdirectory.
PROD_ROOT="$TMP/prodhouse"
SPIRA_PROD="$PROD_ROOT/spira"
git init -q "$PROD_ROOT"
git -C "$PROD_ROOT" config user.email t@t; git -C "$PROD_ROOT" config user.name t

tag_release() {
    echo "$1" > "$PROD_ROOT/f"
    git -C "$PROD_ROOT" add f
    git -C "$PROD_ROOT" commit -q -m "release $1"
    git -C "$PROD_ROOT" tag "spira-release-spira-$1"
}

# ======================================================================================
echo "no acceptance ref at all renders NEVER, not FAIL and not 0:"

tag_release 20260101T000000Z
tag_release 20260102T000000Z
out="$(run_unsent SPIRA_PROD="$SPIRA_PROD")"
want "verdict is NEVER"       "SP_ACCEPT_VERDICT=NEVER" "$out"
want "since counts both tags" "SP_ACCEPT_SINCE=2"       "$out"
nowant "never is not FAIL"    "SP_ACCEPT_VERDICT=FAIL"  "$out"

# ======================================================================================
echo
echo "a stale FAIL note renders FAIL plus releases cut since it:"

first_tag="spira-release-spira-20260101T000000Z"
note_obj="$(git -C "$PROD_ROOT" rev-parse "$first_tag")"
git -C "$PROD_ROOT" notes --ref=acceptance add -m "FAIL: install refused" "$note_obj"
tag_release 20260103T000000Z
out="$(run_unsent SPIRA_PROD="$SPIRA_PROD")"
want "verdict is FAIL"           "SP_ACCEPT_VERDICT=FAIL"                  "$out"
want "tag is the noted one"      "SP_ACCEPT_TAG=$first_tag"                "$out"
want "since counts the two cut after it" "SP_ACCEPT_SINCE=2"               "$out"

# ======================================================================================
echo
echo "an unreadable SPIRA_PROD renders ? everywhere, never 0:"

out="$(run_unsent SPIRA_PROD="$TMP/does-not-exist/spira")"
want "verdict is ?" "SP_ACCEPT_VERDICT=?" "$out"
want "since is ?"   "SP_ACCEPT_SINCE=?"   "$out"
nowant "unreadable is not zero releases" "SP_ACCEPT_SINCE=0" "$out"

# ======================================================================================
echo
echo "a fresh PASS with nothing cut since it renders SP_ACCEPT_SINCE=0:"

last_tag="spira-release-spira-20260103T000000Z"
last_obj="$(git -C "$PROD_ROOT" rev-parse "$last_tag")"
git -C "$PROD_ROOT" notes --ref=acceptance add -m "PASS: installed clean" "$last_obj"
out="$(run_unsent SPIRA_PROD="$SPIRA_PROD")"
want "verdict is PASS" "SP_ACCEPT_VERDICT=PASS" "$out"
want "since is 0"      "SP_ACCEPT_SINCE=0"      "$out"

# ======================================================================================
echo
echo "health.sh keeps NEVER, FAIL and ? distinct in the rendered row:"

render_row() {
    cat > "$RUN/cockpit.env" <<SNAP
SP_AT='1788845700'
SP_ACCEPT_VERDICT='$1'
SP_ACCEPT_TAG='$2'
SP_ACCEPT_AT='$3'
SP_ACCEPT_SINCE='$4'
SP_SENTINEL_TIMER='1'
SP_SENTINEL_AGE='10'
SP_OPS_TIMER='1'
SP_OPS_AGE='10'
SNAP
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 TERM=dumb \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        bash "$(cd "$(dirname "$0")/../cockpit" && pwd)/health.sh" once 2>/dev/null
}

strip_ansi() { sed -E 's/\x1b\[[0-9;]*[a-zA-Z]//g'; }

never_out="$(render_row NEVER - '?' 2 | strip_ansi)"
fail_out="$(render_row FAIL "$first_tag" 1788800000 2 | strip_ansi)"
unknown_out="$(render_row '?' - '?' '?' | strip_ansi)"
want "NEVER row says NEVER"  "NEVER"  "$never_out"
want "FAIL row says FAIL"    "FAIL"   "$fail_out"
want "unknown row says ?"    "accept ?" "$unknown_out"
nowant "NEVER row does not say FAIL" "FAIL" "$never_out"
nowant "FAIL row does not say NEVER" "NEVER" "$fail_out"
nowant "unknown row does not say FAIL" "FAIL" "$unknown_out"

# ======================================================================================
echo
# ======================================================================================
echo
echo "the NEWEST noted release wins, whatever order the note objects' ids sort in (sp-m5ka8):"

# `git notes list` is ordered by object id. Force the older release's commit id to sort
# AFTER the newer one's, so a probe that takes the list's last line reads the old FAIL.
P2="$TMP/prod2"; git init -q "$P2"; git -C "$P2" config user.email t@t; git -C "$P2" config user.name t
empty="$(git -C "$P2" mktree </dev/null)"
new_c="$(git -C "$P2" commit-tree "$empty" -m "release new")"
i=0; old_c=""
while :; do
    old_c="$(git -C "$P2" commit-tree "$empty" -m "release old $i")"
    [[ "$old_c" > "$new_c" ]] && break
    i=$((i+1)); [ "$i" -lt 200 ] || break
done
want "fixture: the older release's id sorts after the newer one's" "yes" \
    "$([[ "$old_c" > "$new_c" ]] && echo yes || echo no)"
git -C "$P2" tag spira-release-spira-20260201T000000Z "$old_c"
git -C "$P2" tag spira-release-spira-20260202T000000Z "$new_c"
git -C "$P2" notes --ref=acceptance add -m "FAIL: install refused" "$old_c"
git -C "$P2" notes --ref=acceptance add -m "PASS: installed clean" "$new_c"
out="$(run_unsent SPIRA_PROD="$P2/spira")"
want "newest noted release's verdict" "SP_ACCEPT_VERDICT=PASS" "$out"
want "and its tag"                    "SP_ACCEPT_TAG=spira-release-spira-20260202T000000Z" "$out"
want "nothing cut since it"           "SP_ACCEPT_SINCE=0" "$out"

tl_summary
