#!/usr/bin/env bash
#
# test-render-memories.sh — tiered render_memories: core full text, index slugs.
#
# WHAT IS UNDER TEST (sp-4e69e)
# ------------------------------
# render_memories in lib.sh now renders in two tiers:
#   Core tier   — statutes in SPIRA_STATUTE_CORE render in full (## slug + paragraph).
#   Index tier  — all other statutes render as slug lines under a "bind equally" heading.
#
# ACCEPTANCE CRITERIA EXERCISED
# ------------------------------
#   1. Core statute renders in full; non-core renders as slug line; neither missing.
#   2. Count of rendered slugs == count of statutes in force.
#   3. Growth assertion: +1 statute → delta is ONE LINE, not one paragraph.
#   4. Negative control: removing a slug from SPIRA_STATUTE_CORE moves it to the index tier.
#   5. Budget: a core statute too large for the char-budget falls back to index, not vanishes.
#   6. Token count against the live book is not a suite concern; see bead acceptance criteria
#      for the human-verified measurement.
#
# PAIRS (law-absence-needs-a-positive-control): every negative case has a positive pair.
# REAL DB (law-prefer-the-real-dependency): uses testdb.sh, not a stub.
#
# covers: spira/lib.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require test-render-memories
testdb_up render-memories || { echo "testdb_up failed" >&2; exit 1; }

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]]  && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

# Source lib.sh with the fixture database in force.
LIB_SH="$HERE/lib.sh"
[ -f "$LIB_SH" ] || { echo "SKIP lib.sh not found" >&2; exit 77; }

# Helper: run render_memories with controlled env.
run_render() {
    local core_csv="$1" prefixes="${2:-law-}" budget="${3:-120000}"
    SPIRA_DB="$SPIRA_DB" SPIRA_BD="$SPIRA_BD" \
    SPIRA_HOME="$HERE" \
    SPIRA_STATUTE_CORE="$core_csv" \
    SPIRA_MEMORIES_CACHE="" \
    bash -c ". \"$LIB_SH\" && render_memories \"$prefixes\" \"$budget\""
}

# Seed a small, controlled set of law- memories so tests are deterministic.
# All seeded with $SPIRA_BD so we address the fixture, not production.
seed_law() {
    local key="$1" body="$2"
    "$SPIRA_BD" -C "$SPIRA_DB" remember --key "$key" "$body" >/dev/null 2>&1
}

seed_law "law-rm-alpha"   "Alpha statute body. This is the full text of alpha."
seed_law "law-rm-beta"    "Beta statute body. This is the full text of beta."
seed_law "law-rm-gamma"   "Gamma statute body. This is the full text of gamma."

echo "test-render-memories.sh"

# ==========================================================================
echo
echo "=== core vs index: basic routing ==="
# ==========================================================================

# POSITIVE CONTROL: alpha is in core → full text appears; beta is not → slug only.
out_basic="$(run_render "law-rm-alpha" "law-rm-")"

# Core statute: heading and body text present.
want  "core: ## heading present"       "## law-rm-alpha"  "$out_basic"
want  "core: body text present"        "Alpha statute body" "$out_basic"

# Non-core statute: slug line present, but NOT its body.
want  "index: beta slug present"       "law-rm-beta"   "$out_basic"
nowant "index: beta body absent"       "Beta statute body" "$out_basic"

# NEGATIVE CONTROL: flip core to beta → alpha goes to index.
out_flip="$(run_render "law-rm-beta" "law-rm-")"
want  "flipped: beta body present"     "Beta statute body"  "$out_flip"
nowant "flipped: alpha body absent"    "Alpha statute body" "$out_flip"
want  "flipped: alpha slug present"    "law-rm-alpha"       "$out_flip"

# ==========================================================================
echo
echo "=== slug count == statute count ==="
# ==========================================================================

# With alpha in core: all three slugs must appear somewhere in the output.
out_all="$(run_render "law-rm-alpha" "law-rm-")"
want "all: alpha appears"   "law-rm-alpha"  "$out_all"
want "all: beta appears"    "law-rm-beta"   "$out_all"
want "all: gamma appears"   "law-rm-gamma"  "$out_all"

# Count occurrences of "law-rm-" — each slug appears exactly once.
count="$(printf '%s\n' "$out_all" | grep -c "^law-rm-\|^## law-rm-")"
if [ "$count" -eq 3 ]; then
    ok "slug count == statute count (3)"
else
    bad "slug count == statute count" "got $count, want 3"
fi

# ==========================================================================
echo
echo "=== growth assertion: +1 statute = +1 line, not +1 paragraph ==="
# ==========================================================================

# Render with alpha+beta in core and gamma in index, count lines.
out_before="$(run_render "law-rm-alpha,law-rm-beta" "law-rm-")"
lines_before="$(printf '%s\n' "$out_before" | wc -l | tr -d ' ')"

# Add one new index statute and re-render.
seed_law "law-rm-delta" "Delta statute body. Full text of delta."
out_after="$(run_render "law-rm-alpha,law-rm-beta" "law-rm-")"
lines_after="$(printf '%s\n' "$out_after" | wc -l | tr -d ' ')"
delta=$((lines_after - lines_before))

# Delta should be exactly 1 (one slug line added to the index tier).
if [ "$delta" -eq 1 ]; then
    ok "growth assertion: +1 statute = +1 line (delta=$delta)"
else
    bad "growth assertion: +1 statute = +1 line" "delta=$delta (before=$lines_before after=$lines_after)"
fi

# Confirm delta statute body NOT in output (it was added as an index statute).
nowant "growth: delta body absent" "Delta statute body" "$out_after"
want   "growth: delta slug present" "law-rm-delta" "$out_after"

# ==========================================================================
echo
echo "=== budget fallback: core too large falls to index, not vanishes ==="
# ==========================================================================

# Set a tiny budget (40 chars) so alpha's full block won't fit.
# Alpha body is "Alpha statute body. This is the full text of alpha." — block > 40 chars.
out_budget="$(run_render "law-rm-alpha,law-rm-beta" "law-rm-" "40")"

# Alpha must still appear as a slug (fallback to index), not disappear.
want  "budget fallback: alpha slug present" "law-rm-alpha" "$out_budget"
nowant "budget fallback: alpha body absent" "Alpha statute body" "$out_budget"

# ==========================================================================
echo
echo "=== index heading present when index tier is non-empty ==="
# ==========================================================================

out_heading="$(run_render "law-rm-alpha" "law-rm-")"
want "index heading: bind-equally text" "Statutes in force" "$out_heading"
want "index heading: rule.sh command"   "rule.sh show"      "$out_heading"
_rule_path="$(printf '%s\n' "$out_heading" | sed -n 's|^ *\(/[^ ]*rule\.sh\) show.*|\1|p' | head -1)"
if [ -x "${_rule_path:-}" ]; then
    ok "index heading: rule.sh path is executable"
else
    bad "index heading: rule.sh path is executable" "not executable: [${_rule_path:-<not found>}]"
fi

# ==========================================================================
echo
echo "=== empty core: all slugs go to index ==="
# ==========================================================================

out_empty_core="$(run_render "" "law-rm-")"
# No ## headings with bodies — every statute is in index tier.
nowant "empty core: alpha body absent" "Alpha statute body" "$out_empty_core"
want   "empty core: alpha slug present" "law-rm-alpha" "$out_empty_core"
want   "empty core: heading present" "Statutes in force" "$out_empty_core"

# ==========================================================================
echo
echo "=== full core: all slugs get full text, no index tier ==="
# ==========================================================================

out_full_core="$(run_render "law-rm-alpha,law-rm-beta,law-rm-gamma,law-rm-delta" "law-rm-")"
want  "full core: alpha body present" "Alpha statute body" "$out_full_core"
want  "full core: beta body present"  "Beta statute body"  "$out_full_core"
# No index tier heading when nothing goes there.
nowant "full core: no index heading" "Statutes in force" "$out_full_core"

# ==========================================================================
echo
echo "=== cache: hit, miss, and write ==="
# ==========================================================================

# run_render_cached uses a temp cache file instead of disabling caching, so we can
# verify the cache is read on a hit and bypassed on a miss (stale or absent).
CACHE_FILE="$TMP/test-memories-cache.json"
run_render_cached() {
    local core_csv="$1" prefixes="${2:-law-rm-}" age="${3:-300}"
    SPIRA_DB="$SPIRA_DB" SPIRA_BD="$SPIRA_BD" \
    SPIRA_HOME="$HERE" \
    SPIRA_STATUTE_CORE="$core_csv" \
    SPIRA_MEMORIES_CACHE="$CACHE_FILE" \
    SPIRA_MEMORIES_CACHE_AGE="$age" \
    bash -c ". \"$LIB_SH\" && render_memories \"$prefixes\""
}

# CACHE MISS → CACHE WRITE. No cache file exists; live read must happen and the file
# must be written. The positive control for the write check below.
rm -f "$CACHE_FILE"
out_miss="$(run_render_cached "law-rm-alpha" "law-rm-")"
want "cache miss: live read renders alpha" "Alpha statute body" "$out_miss"
if [ -f "$CACHE_FILE" ]; then
    ok "cache miss: cache file written after live read"
else
    bad "cache miss: cache file written after live read" "file absent: $CACHE_FILE"
fi

# CACHE HIT: write a cache file with a DISTINCT body ("CACHED") so we can tell which
# source render_memories used. The db has "Alpha statute body"; the cache has "CACHED
# alpha body". If the cache is read, the render returns the cache body; if the live
# db is read, it returns the db body. conf.sh's bd migrate schema check still runs
# (it uses the real $SPIRA_BD and $SPIRA_DB), so bd is never absent.
printf '{"law-rm-alpha": "CACHED alpha body", "law-rm-beta": "Beta statute body."}\n' > "$CACHE_FILE"
touch "$CACHE_FILE"  # reset mtime so age check sees a fresh file
# POSITIVE CONTROL: the cache file must hold the distinct marker for this to prove anything.
if grep -q "CACHED alpha body" "$CACHE_FILE"; then
    ok "cache hit: positive control — cache holds distinct marker"
    out_hit="$(run_render_cached "law-rm-alpha")"
    want   "cache hit: renders CACHED body from cache" "CACHED alpha body" "$out_hit"
    nowant "cache hit: does not render db body"         "Alpha statute body" "$out_hit"
else
    bad "cache hit: positive control" "marker not in cache file"
fi

# STALE CACHE (age=0) → LIVE READ. With age=0, every cache is expired. The live db
# returns the real body; the stale file must be ignored.
out_stale="$(run_render_cached "law-rm-alpha" "law-rm-" "0")"
want   "stale cache: live read returns db body"      "Alpha statute body" "$out_stale"
nowant "stale cache: does not return cached body"    "CACHED alpha body"  "$out_stale"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
