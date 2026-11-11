#!/usr/bin/env bash
#
# test-overrides-landref.sh — a declared override retires off the repo's landing ref, never a
# checkout's HEAD; an unlanded override is held in place; a spec naming a bead the store does
# not know is a loud error.
#
# sp-d437vt's cases. test-overrides.sh, where they were first written, was deleted by the
# 2026-10-31 test review (sp-ydc08l); its own skew-refresh cases are not carried here.
#
# defect: sp-d437vt
# tier: T3
# covers: spira/overrides.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"

echo "test-overrides-landref.sh"

testdb_require test-overrides-landref
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up overrides_landref || { printf 'test-overrides-landref: could not build fixture database\n' >&2; exit 1; }
testdb_seed <<'JSONL'
{"id":"sp-testlag","title":"lagging checkout target","status":"open","issue_type":"task","labels":["repo:spira"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-testheld","title":"unlanded target","status":"open","issue_type":"task","labels":["repo:spira"],"updated_at":"2026-09-25T00:00:00Z"}
JSONL

# ── Fixture: a remote, a checkout of it, and the repo map naming origin/main as its landing ref
ORIGIN="$TMP/origin"
REPO="$TMP/repo"
git init -q -b main "$ORIGIN"
git -C "$ORIGIN" config user.email "test@test"
git -C "$ORIGIN" config user.name "test"
printf 'base\n' > "$ORIGIN/f"
git -C "$ORIGIN" add f
git -C "$ORIGIN" commit -q -m "base"
timeout 5 git clone -q "$ORIGIN" "$REPO"

RMAP="$TMP/repomap"
RUN="$TMP/run"; mkdir -p "$RUN"
printf 'fixture | %s | push | origin/main | | |\n' "$REPO" > "$RMAP"

run_ov() { # run_ov <spec dir> <verb>
    tl_config SPIRA_RUN="$RUN" SPIRA_OVERRIDES="$1" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$RMAP" SPIRA_CONCIERGE_INBOX="$TMP/inbox.log"
    env -i SPIRA_RELEASE="$(tl_release)" SPIRA_TOML="$SPIRA_TOML" PATH="$PATH" HOME="$TMP/home" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
        overrides.sh "$2" "$REPO" 2>&1
}

# ===========================================================================
echo
echo "a land commit on the landing ref retires the spec though the checkout's HEAD lags:"
# ===========================================================================
# Landings advance the ref with plumbing; the checkout is never moved. Seen red against the
# HEAD-reading check: the checkout's own log has no land commit.

LAGDIR="$TMP/overrides-lag"; mkdir -p "$LAGDIR"
cat > "$LAGDIR/lag.override" <<'SPEC'
BEAD="sp-testlag"
needed() { [ ! -f "$1/lag-applied" ]; }
apply()  { : > "$1/lag-applied"; }
on_landed() { printf 'ran\n' >> "$1/.lag-landed"; }
SPEC
out="$(run_ov "$LAGDIR" apply)"
want "unlanded bead: applied when needed says the edit is missing" "lag: applied" "$out"
rm -f "$REPO/lag-applied"
run_ov "$LAGDIR" apply >/dev/null
[ -f "$REPO/lag-applied" ] \
    && ok  "unlanded bead: re-applied when the edit goes missing again" \
    || bad "unlanded bead: re-applied when the edit goes missing again" "no lag-applied"

git -C "$ORIGIN" commit -q --allow-empty -m "spira: land sp-testlag — lagging"
timeout 5 git -C "$REPO" fetch -q origin
nowant "fixture: the checkout HEAD does not carry the land commit" "sp-testlag" "$(git -C "$REPO" log HEAD --format=%s)"
out="$(run_ov "$LAGDIR" apply)"
want "lagging checkout: retired off the landing ref" "lag: retired" "$out"
is   "lagging checkout: on_landed ran once" "1" "$(wc -l < "$REPO/.lag-landed")"
[ -f "$LAGDIR/retired/lag.override" ] && ok "lagging checkout: spec moved to retired/" || bad "lagging checkout: spec moved to retired/"
run_ov "$LAGDIR" apply >/dev/null
is   "lagging checkout: a second pass does not run on_landed again" "1" "$(wc -l < "$REPO/.lag-landed")"
want "retirement is logged to the Concierge inbox" "lag retired: sp-testlag landed" "$(cat "$TMP/inbox.log" 2>/dev/null)"

# ===========================================================================
echo
echo "a spec naming a bead the store does not know is a loud error:"
# ===========================================================================
GHOSTDIR="$TMP/overrides-ghost"; mkdir -p "$GHOSTDIR"
cat > "$GHOSTDIR/ghost.override" <<'SPEC'
BEAD="sp-nosuchbead"
needed() { return 0; }
apply()  { : > "$1/ghost-applied"; }
SPEC
out="$(run_ov "$GHOSTDIR" apply)"
want "unknown bead: named in the error" "sp-nosuchbead is not in the store" "$out"
[ ! -e "$REPO/ghost-applied" ] && ok "unknown bead: nothing applied" || bad "unknown bead: nothing applied"
want "unknown bead: doctor reports it" "ghost: sp-nosuchbead failed — unknown-bead" "$(run_ov "$GHOSTDIR" doctor)"

# ===========================================================================
echo
echo "the store checks read through spira-lc content, never bdq:"
# ===========================================================================
STUBBIN="$TMP/stubbin"; mkdir -p "$STUBBIN"
lc_path_stub "$STUBBIN" "$TMP/lcfix"
printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> "%s/bdq.calls"\nexit 1\n' "$TMP" > "$STUBBIN/bdq"; chmod +x "$STUBBIN/bdq"
: > "$TMP/bdq.calls"
NOBDQDIR="$TMP/overrides-nobdq"; mkdir -p "$NOBDQDIR"
cat > "$NOBDQDIR/held.override" <<'SPEC'
BEAD="sp-testheld"
needed() { [ ! -f "$1/held-applied" ]; }
apply()  { : > "$1/held-applied"; }
SPEC
cat > "$NOBDQDIR/ghost.override" <<'SPEC'
BEAD="sp-nosuchbead"
needed() { return 0; }
apply()  { : > "$1/ghost2-applied"; }
SPEC
out="$(PATH="$STUBBIN:$PATH" run_ov "$NOBDQDIR" apply)"
want "no bdq: known bead is applied" "held: applied" "$out"
want "no bdq: unknown bead is the loud error" "sp-nosuchbead is not in the store" "$out"
[ ! -e "$REPO/ghost2-applied" ] && ok "no bdq: unknown bead applies nothing" || bad "no bdq: unknown bead applies nothing"
is "no bdq: no bdq process was started" "" "$(cat "$TMP/bdq.calls")"

echo
tl_summary
