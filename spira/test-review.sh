#!/usr/bin/env bash
#
# test-review.sh — review.sh: adversarial review of a release unit's aggregate diff.
#
#   ./test-review.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# review.sh reviews a release tag and returns a verdict (ship/block). Five
# properties are checked:
#
#   1. SHIP: a tag reviewed by a clean model response exits 0 with verdict=ship
#      and cost fields recorded in the verdict file.
#   2. BLOCK: a tag reviewed by a model that returns BLOCK exits 1 with
#      verdict=block and finding beads filed in the test database.
#   3. VERDICT COMMAND: review.sh verdict <tag> reads the stored verdict file.
#   4. DEDUPLICATION: re-running a review for the same unit does not file the
#      same finding twice.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# --------------------------------------------------------
# The suite verifies the fake claude is actually called by asserting that cost
# fields in the verdict file match the fake's fixed values — not merely that the
# file exists. It also verifies the suite itself fails when review.sh is absent.
#
# FAKE CLAUDE (law-gates-run-in-a-clean-environment)
# --------------------------------------------------
# A real claude call is prohibited in a suite: it draws on the operator's account,
# varies by model availability, and costs money on every CI run. The fake claude is
# a Python script set as SPIRA_AGENT in env -i invocations. It reads
# FAKE_REVIEW_VERDICT and emits a minimal valid compact stream-json response.
# Compact JSON is required: the real Claude CLI outputs without spaces, and the
# Python parser in review.sh searches for the literal '"type":"result"'.
#
# defect: sp-gsmx.4
# tier: T2
# covers: spira/review.sh spira/conf.sh spira/lib.sh spira/release.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# ---- test database (real bd, throwaway database) ----------------------------
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-review
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up review || { echo "test-review: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# ---- git fixture ------------------------------------------------------------
# A bare remote with 'trunk' as its default branch (deliberate: a hardcoded
# 'main' in any of the reviewed scripts fails here immediately).
ORIGIN="$TMP/origin.git"
REPO="$TMP/repo"
git init -q --bare -b trunk "$ORIGIN"
git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t
git -C "$REPO" config user.name t

printf 'base\n' > "$REPO/f"
git -C "$REPO" add f
git -C "$REPO" commit -qm "initial commit"
git -C "$REPO" push -q origin trunk 2>/dev/null

printf 'a\n' >> "$REPO/f"; git -C "$REPO" add f
git -C "$REPO" commit -qm "sp-aaa — first bead"

printf 'b\n' >> "$REPO/f"; git -C "$REPO" add f
git -C "$REPO" commit -qm "sp-bbb — second bead"

git -C "$REPO" push -q origin trunk 2>/dev/null

# ---- harness fixture -------------------------------------------------------
SH="$TMP/spira"
mkdir -p "$SH"
for f in review.sh release.sh unhold.sh lib.sh conf.sh suite-covers.sh; do
    [ -f "$HERE/$f" ] && cp "$HERE/$f" "$SH/"
done
chmod +x "$SH/review.sh" "$SH/release.sh"

REPO_MAP="$TMP/repo-map"
# Columns: name | path | land-mode | base | prefix | format
printf 'fixture | %s | push | origin/trunk | sp | |\n' "$REPO" > "$REPO_MAP"

VERDICTS="$TMP/verdicts"

# ---- fake claude ------------------------------------------------------------
# Emits compact stream-json (separators=(',',':')) so the '"type":"result"'
# substring check in review.sh finds the result event. Python's default
# json.dumps adds spaces after ':' which would break the check.
FAKE_CLAUDE="$TMP/fake-claude"
cat > "$FAKE_CLAUDE" <<'FAKEEOF'
#!/usr/bin/env python3
import json, os, sys
sys.stdin.read()  # consume the prompt
verdict = os.environ.get("FAKE_REVIEW_VERDICT", "ship")
if verdict == "block":
    result_text = (
        "VERDICT: BLOCK\n"
        "FINDING: remove-safety-check in deploy.sh\n"
        "Line 42 of deploy.sh removes the --dry-run guard that prevented\n"
        "accidental production writes. No replacement was added.\n"
        "---\n"
    )
else:
    result_text = "VERDICT: SHIP"
events = [
    {"type":"system","subtype":"init","session_id":"test",
     "tools":[],"mcp_servers":[]},
    {"type":"result","subtype":"success","is_error":False,
     "result":result_text,"session_id":"test",
     "total_cost_usd":0.0042,
     "duration_ms":100,"duration_api_ms":80,"num_turns":1,
     "usage":{"input_tokens":512,"output_tokens":18,
              "cache_read_input_tokens":0}},
]
for e in events:
    # Compact separators: the real Claude CLI outputs without spaces, and
    # review.sh's Python parser searches for '"type":"result"' literally.
    print(json.dumps(e, separators=(',', ':')))
FAKEEOF
chmod +x "$FAKE_CLAUDE"

# ---- helpers ----------------------------------------------------------------
run_review() {
    env -i PATH="$PATH" HOME="$HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" \
        SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_REPO="$SH" \
        SPIRA_GOAL=sp-goal \
        SPIRA_ID_PREFIX=sp \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_AGENT="$FAKE_CLAUDE" \
        SPIRA_REVIEWER_VERDICTS="$VERDICTS" \
        SPIRA_REVIEWER_MODEL=claude-test-model \
        SPIRA_REVIEWER_TIMEOUT=30 \
        FAKE_REVIEW_VERDICT="${FAKE_REVIEW_VERDICT:-ship}" \
        bash "$SH/review.sh" "$@" 2>&1
}

run_release() {
    env -i PATH="$PATH" HOME="$HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" \
        SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_REPO="$SH" \
        SPIRA_GOAL=sp-goal \
        SPIRA_ID_PREFIX=sp \
        bash "$SH/release.sh" "$@" 2>&1
}

mkdir -p "$TMP/run" "$VERDICTS"

# Cut a release tag for the fixture repo.
TAG="$(run_release cut fixture)"
[ -n "$TAG" ] || { echo "SETUP FAILED: could not cut release tag"; exit 1; }

# ======================================================================================
echo
echo "1. SHIP — clean review exits 0, verdict file written with cost fields"
# ======================================================================================
FAKE_REVIEW_VERDICT=ship
out_ship="$(run_review "$TAG")"
rc_ship=$?

is   "ship: exit 0"              0      "$rc_ship"
want "ship: stdout contains ship" "ship" "$out_ship"

vfile="$VERDICTS/$TAG.verdict"
is   "ship: verdict file exists"     "true"           "$([ -f "$vfile" ] && echo true || echo false)"
want "ship: verdict=ship"            "verdict: ship"  "$(cat "$vfile" 2>/dev/null)"
want "ship: model field"             "model: "        "$(cat "$vfile" 2>/dev/null)"
want "ship: timestamp field"         "timestamp: "    "$(cat "$vfile" 2>/dev/null)"
want "ship: cost_usd field"          "cost_usd: "     "$(cat "$vfile" 2>/dev/null)"
want "ship: in_tok field"            "in_tok: "       "$(cat "$vfile" 2>/dev/null)"
want "ship: out_tok field"           "out_tok: "      "$(cat "$vfile" 2>/dev/null)"
want "ship: findings: 0"             "findings: 0"    "$(cat "$vfile" 2>/dev/null)"
# POSITIVE CONTROL: cost_usd must match the fake claude's fixed value, proving
# the fake was called and its JSON was parsed — not merely that the file exists.
want "ship: cost_usd matches fake"   "0.0042"  "$(cat "$vfile" 2>/dev/null)"
want "ship: in_tok matches fake"     "512"     "$(cat "$vfile" 2>/dev/null)"

# ======================================================================================
echo
echo "2. VERDICT COMMAND — review.sh verdict reads the stored verdict"
# ======================================================================================
vout="$(run_review verdict "$TAG")"
is "verdict cmd: ship tag returns ship"            "ship" "$vout"

vout_none="$(run_review verdict "spira-release-nosuch-20260101T000000Z" 2>&1 || true)"
is "verdict cmd: unknown tag returns none"         "none"  "$vout_none"

# ======================================================================================
echo
echo "3. BLOCK — review exits 1 and files findings as beads"
# ======================================================================================
# Cut a second release tag (one new commit) for the block scenario.
printf 'c\n' >> "$REPO/f"; git -C "$REPO" add f
git -C "$REPO" commit -qm "sp-ccc — third bead"
git -C "$REPO" push -q origin trunk 2>/dev/null
TAG2="$(run_release cut fixture)"
[ -n "$TAG2" ] || { echo "SETUP FAILED: could not cut second release tag"; exit 1; }

FAKE_REVIEW_VERDICT=block run_review "$TAG2" >/dev/null 2>&1; rc_block=$?
block_stdout="$(FAKE_REVIEW_VERDICT=block run_review "$TAG2" 2>&1 || true)"

is   "block: exit 1"                  1       "$rc_block"
want "block: stdout contains block"  "block"  "$block_stdout"

vfile2="$VERDICTS/$TAG2.verdict"
want "block: verdict=block"    "verdict: block"  "$(cat "$vfile2" 2>/dev/null)"
want "block: findings > 0"     "findings: 1"     "$(cat "$vfile2" 2>/dev/null)"

# A finding bead must have been filed in the database.
db_count() {
    env -i PATH="$PATH" HOME="$HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_ID_PREFIX=sp \
        bash -c '. '"$SH/lib.sh"'; bdjson list --status open --label review-finding --limit 0 2>/dev/null | python3 -c "
import json,sys
try:
    d=json.load(sys.stdin)
    print(len(d) if isinstance(d,list) else (1 if d else 0))
except: print(0)
"'
}
bead_count="$(db_count)"
[ "$bead_count" -ge 1 ] \
    && ok "block: finding bead filed (count=$bead_count)" \
    || bad "block: finding bead filed" "got count=$bead_count, want >=1"

# The bead title must name the finding.
bead_titles="$(env -i PATH="$PATH" HOME="$HOME" \
    SPIRA_CONF=/nonexistent \
    SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="$SPIRA_BD" \
    SPIRA_ID_PREFIX=sp \
    bash -c '. '"$SH/lib.sh"'; bdjson list --status open --label review-finding --limit 0 2>/dev/null | python3 -c "
import json,sys
try:
    d=json.load(sys.stdin)
    items=d if isinstance(d,list) else [d]
    for i in items: print(i.get(\"title\",\"\"))
except: pass
"')"
want "block: bead title has 'review finding:'" "review finding:" "$bead_titles"

# ======================================================================================
echo
echo "4. DEDUPLICATION — re-reviewing the same unit does not double-file findings"
# ======================================================================================
FAKE_REVIEW_VERDICT=block run_review "$TAG2" >/dev/null 2>&1 || true
bead_count2="$(db_count)"
is "dedup: count unchanged after re-review"  "$bead_count"  "$bead_count2"

# ======================================================================================
echo
echo "5. SUITE SELF-CHECK — suite fails without review.sh"
# ======================================================================================
rm -f "$SH/review.sh"
out_absent="$(run_review "$TAG" 2>&1 || true)"
nowant "review absent: no ship output"  "ship"  "$out_absent"
tl_summary
