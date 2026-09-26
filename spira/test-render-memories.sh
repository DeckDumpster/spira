#!/usr/bin/env bash
#
# test-render-memories.sh — tiered render_memories: core full text, index slugs, cache,
#   and the delivery fence (every fayth-composed entry point carries the section at all).
#
# WHAT IS UNDER TEST (sp-4e69e)
# ------------------------------
# render_memories in lib.sh renders in two tiers:
#   Core tier   — statutes in SPIRA_STATUTE_CORE render in full (## slug + paragraph).
#   Index tier  — all other statutes render as slug lines under a "bind equally" heading.
#
# THE SEAM. SPIRA_MEMORIES_CMD stands in for the live `bd memories --json` read that fills
# the cache; every case below except one drives it from a fixture, so the tiering, budget
# and cache logic is exercised without a database (UC-operator-channel-42). The one
# real-bd case is the positive control that the seam and the live path agree.
#
# G-08 — THE DELIVERY FENCE. CLAUDE.md records that a hand-started session gets no
# statutes at all: render_memories is called only from aeon.sh (via system_prompt_split)
# and concierge.sh (via compose_brief). Nothing asserted that either entry point actually
# carries the "# Memories in force" section — a silent regression in either wrapper would
# leave a session governed by nothing, looking identical to one that read the law.
#
# NAMESPACES. The index tier serves two prefixes (law-, sop-) but one hardcoded header
# named only rule.sh, so a sop- slug in the index tier was unfetchable by the command its
# own header named (sp-cvpp0). Each namespace now gets a header naming its own retrieval
# command, asserted under its own heading below.
#
# PAIRS (law-absence-needs-a-positive-control): every negative case has a positive pair.
#
# tier: T2
# covers: spira/lib.sh spira/conf.sh spira/aeon.sh UC-operator-channel-42 G-08
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

LIB_SH="$HERE/lib.sh"

# ==========================================================================
echo "=== the seam: SPIRA_MEMORIES_CMD stands in for a live bd read ==="
# ==========================================================================

# A fixture command that prints a fixed JSON object of {slug: body}. Built fresh per call
# so each section can vary the fixture without fighting a shared file.
fixture_cmd() { # fixture_cmd <json> -> a shell command string render_memories can run
    local json="$1" f="$TMP/fixture-$$-$RANDOM.json"
    printf '%s' "$json" > "$f"
    printf 'cat %q' "$f"
}

run_render() { # run_render <json> <core-csv> [prefixes] [budget]
    local json="$1" core_csv="$2" prefixes="${3:-law-rm-}" budget="${4:-120000}"
    SPIRA_HOME="$HERE" SPIRA_STATUTE_CORE="$core_csv" SPIRA_MEMORIES_CACHE="" \
        SPIRA_MEMORIES_CMD="$(fixture_cmd "$json")" \
        bash -c ". \"$LIB_SH\" && render_memories \"$prefixes\" \"$budget\""
}

FIX3='{"law-rm-alpha":"Alpha statute body. This is the full text of alpha.","law-rm-beta":"Beta statute body. This is the full text of beta.","law-rm-gamma":"Gamma statute body. This is the full text of gamma."}'

# POSITIVE CONTROL: alpha is in core → full text appears; beta is not → slug only.
out_basic="$(run_render "$FIX3" "law-rm-alpha")"
want   "core: ## heading present"     "## law-rm-alpha"    "$out_basic"
want   "core: body text present"      "Alpha statute body" "$out_basic"
want   "index: beta slug present"     "law-rm-beta"         "$out_basic"
nowant "index: beta body absent"      "Beta statute body"   "$out_basic"

# NEGATIVE CONTROL: flip core to beta → alpha goes to index.
out_flip="$(run_render "$FIX3" "law-rm-beta")"
want   "flipped: beta body present"   "Beta statute body"   "$out_flip"
nowant "flipped: alpha body absent"   "Alpha statute body"  "$out_flip"
want   "flipped: alpha slug present"  "law-rm-alpha"        "$out_flip"

echo
echo "=== slug count == statute count ==="

out_all="$(run_render "$FIX3" "law-rm-alpha")"
want "all: alpha appears"  "law-rm-alpha" "$out_all"
want "all: beta appears"   "law-rm-beta"  "$out_all"
want "all: gamma appears"  "law-rm-gamma" "$out_all"
count="$(printf '%s\n' "$out_all" | grep -c "^law-rm-\|^## law-rm-")"
is "slug count == statute count (3)" "3" "$count"

echo
echo "=== growth assertion: +1 statute = +1 line, not +1 paragraph ==="

out_before="$(run_render "$FIX3" "law-rm-alpha,law-rm-beta")"
lines_before="$(printf '%s\n' "$out_before" | wc -l | tr -d ' ')"

FIX4='{"law-rm-alpha":"Alpha statute body. This is the full text of alpha.","law-rm-beta":"Beta statute body. This is the full text of beta.","law-rm-gamma":"Gamma statute body. This is the full text of gamma.","law-rm-delta":"Delta statute body. Full text of delta."}'
out_after="$(run_render "$FIX4" "law-rm-alpha,law-rm-beta")"
lines_after="$(printf '%s\n' "$out_after" | wc -l | tr -d ' ')"
is "growth: +1 statute = +1 line" "1" "$((lines_after - lines_before))"
nowant "growth: delta body absent"  "Delta statute body" "$out_after"
want   "growth: delta slug present" "law-rm-delta"        "$out_after"

echo
echo "=== budget fallback: core too large falls to index, not vanishes ==="

out_budget="$(run_render "$FIX3" "law-rm-alpha,law-rm-beta" "law-rm-" "40")"
want   "budget fallback: alpha slug present" "law-rm-alpha"       "$out_budget"
nowant "budget fallback: alpha body absent"  "Alpha statute body" "$out_budget"

echo
echo "=== index heading present when index tier is non-empty ==="

out_heading="$(run_render "$FIX3" "law-rm-alpha")"
want "index heading: bind-equally text" "Statutes in force" "$out_heading"
want "index heading: rule.sh command"   "rule.sh show"      "$out_heading"
_rule_path="$(printf '%s\n' "$out_heading" | sed -n 's|^ *\(/[^ ]*rule\.sh\) show.*|\1|p' | head -1)"
if [ -x "${_rule_path:-}" ]; then
    ok "index heading: rule.sh path is executable"
else
    bad "index heading: rule.sh path is executable" "not executable: [${_rule_path:-<not found>}]"
fi

echo
echo "=== mixed namespaces: law- and sop- each get their own retrieval command ==="
# ==========================================================================

# POSITIVE CONTROL for the offender this suite must catch: a sop- key delivered under a
# header naming rule.sh (which prepends law- and refuses every sop- key) is unfetchable.
FIX_MIXED='{"law-rm-alpha":"Alpha statute body. This is the full text of alpha.","sop-rm-widget":"Widget runbook body."}'
out_mixed="$(run_render "$FIX_MIXED" "" "law-rm-,sop-rm-")"

law_section="$(printf '%s\n' "$out_mixed" | awk '/^## Statutes/{f=1} /^## Runbooks/{f=0} f')"
sop_section="$(printf '%s\n' "$out_mixed" | awk '/^## Runbooks/{f=1} f')"

want   "mixed: sop slug present"            "sop-rm-widget"  "$out_mixed"
want   "mixed: sop.sh retrieval command"    "sop.sh show"    "$out_mixed"
want   "mixed: runbook heading present"     "Runbooks on the shelf" "$out_mixed"
want   "mixed: law slug still present"      "law-rm-alpha"   "$out_mixed"
want   "mixed: rule.sh retrieval command"   "rule.sh show"   "$out_mixed"

# Each namespace's slugs sit under ITS OWN header, not the other one's.
want   "mixed: sop slug under sop header"   "sop-rm-widget"  "$sop_section"
nowant "mixed: sop slug not under law header" "sop-rm-widget" "$law_section"
want   "mixed: law slug under law header"   "law-rm-alpha"   "$law_section"
nowant "mixed: law slug not under sop header" "law-rm-alpha" "$sop_section"

_sop_path="$(printf '%s\n' "$sop_section" | sed -n 's|^ *\(/[^ ]*sop\.sh\) show.*|\1|p' | head -1)"
if [ -x "${_sop_path:-}" ]; then
    ok "mixed: sop.sh path is executable"
else
    bad "mixed: sop.sh path is executable" "not executable: [${_sop_path:-<not found>}]"
fi

# ==========================================================================
echo
echo "=== empty core: all slugs go to index ==="

out_empty_core="$(run_render "$FIX3" "")"
nowant "empty core: alpha body absent"  "Alpha statute body" "$out_empty_core"
want   "empty core: alpha slug present" "law-rm-alpha"        "$out_empty_core"
want   "empty core: heading present"    "Statutes in force"   "$out_empty_core"

echo
echo "=== full core: all slugs get full text, no index tier ==="

out_full_core="$(run_render "$FIX4" "law-rm-alpha,law-rm-beta,law-rm-gamma,law-rm-delta")"
want   "full core: alpha body present" "Alpha statute body" "$out_full_core"
want   "full core: beta body present"  "Beta statute body"  "$out_full_core"
nowant "full core: no index heading"   "Statutes in force"   "$out_full_core"

echo
echo "=== cache: hit, miss, and write ==="

CACHE_FILE="$TMP/test-memories-cache.json"
run_render_cached() { # run_render_cached <json> <core-csv> [age]
    local json="$1" core_csv="$2" age="${3:-300}"
    SPIRA_HOME="$HERE" SPIRA_STATUTE_CORE="$core_csv" SPIRA_MEMORIES_CACHE="$CACHE_FILE" \
        SPIRA_MEMORIES_CACHE_AGE="$age" SPIRA_MEMORIES_CMD="$(fixture_cmd "$json")" \
        bash -c ". \"$LIB_SH\" && render_memories \"law-rm-\""
}

rm -f "$CACHE_FILE"
out_miss="$(run_render_cached "$FIX3" "law-rm-alpha")"
want "cache miss: live read renders alpha" "Alpha statute body" "$out_miss"
if [ -f "$CACHE_FILE" ]; then
    ok "cache miss: cache file written after live read"
else
    bad "cache miss: cache file written after live read" "file absent: $CACHE_FILE"
fi

# CACHE HIT: the cache holds a DISTINCT body so we can tell which source render_memories
# used, without touching the seam command at all for this read.
printf '{"law-rm-alpha": "CACHED alpha body", "law-rm-beta": "Beta statute body."}\n' > "$CACHE_FILE"
touch "$CACHE_FILE"
if grep -q "CACHED alpha body" "$CACHE_FILE"; then
    ok "cache hit: positive control — cache holds distinct marker"
    out_hit="$(SPIRA_HOME="$HERE" SPIRA_STATUTE_CORE="law-rm-alpha" SPIRA_MEMORIES_CACHE="$CACHE_FILE" \
        SPIRA_MEMORIES_CMD="$(fixture_cmd "$FIX3")" \
        bash -c ". \"$LIB_SH\" && render_memories \"law-rm-\"")"
    want   "cache hit: renders CACHED body from cache" "CACHED alpha body"  "$out_hit"
    nowant "cache hit: does not consult the seam"      "Alpha statute body" "$out_hit"
else
    bad "cache hit: positive control" "marker not in cache file"
fi

out_stale="$(run_render_cached "$FIX3" "law-rm-alpha" "0")"
want   "stale cache: live read returns fixture body" "Alpha statute body" "$out_stale"
nowant "stale cache: does not return cached body"    "CACHED alpha body"  "$out_stale"

echo
echo "=== the seam agrees with a real bd read (one real-bd case) ==="

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-render-memories
if testdb_up render-memories >/dev/null 2>&1; then
    "$SPIRA_BD" -C "$SPIRA_DB" remember --key "law-rm-live" \
        "Live statute body, read for real." >/dev/null 2>&1
    out_live="$(SPIRA_DB="$SPIRA_DB" SPIRA_BD="$SPIRA_BD" SPIRA_HOME="$HERE" \
        SPIRA_STATUTE_CORE="law-rm-live" SPIRA_MEMORIES_CACHE="" \
        bash -c ". \"$LIB_SH\" && render_memories \"law-rm-\"")"
    want "a real bd read renders the live statute in full" "Live statute body" "$out_live"
    testdb_drop
else
    bad "real-bd positive control" "testdb_up failed — could not build a fixture database"
fi

echo
echo "=== G-08: the delivery fence — aeon.sh's entry point carries the section ==="

# aeon.sh's entry point is system_prompt_split, called with render_memories's own output.
# It needs no fixture chamber or bd at all — it is a pure function of its arguments. The
# other fayth-composed entry point, concierge.sh's compose_brief, is asserted in
# test-concierge.sh (its brief-composition section already checks for this section; that
# file, unlike this one, declares # requires: claude since compose_brief runs through
# concierge.sh, which refuses to start at all without it).
SYS="$TMP/g08.system.md"; TASK="$TMP/g08.task.md"
( . "$LIB_SH"; system_prompt_split "$SYS" "$TASK" "some statute text" "a prompt<!-- task -->the task" )
want   "aeon.sh path: system.md carries the section" "# Memories in force" "$(cat "$SYS" 2>/dev/null)"
nowant "aeon.sh path: task.md does not"              "# Memories in force" "$(cat "$TASK" 2>/dev/null)"

tl_summary
