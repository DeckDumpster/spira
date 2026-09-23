#!/usr/bin/env bash
#
# test-broker.sh — broker: submit intents, policy enforcement, execute against stub gh.
#
# WHAT THIS COVERS.
#   1. POSITIVE CONTROL: broker binary exists and is executable.
#   2. submit: creates an intent JSON in inbox/ with the correct fields.
#   3. execute: a verb outside the policy table is refused.
#   4. execute: a builder-fayth intent for a czar-only verb is refused.
#   5. execute: czar fayth + run-rerun + shadow → CZAR-WOULD, refused, audited.
#   6. execute: czar fayth + run-rerun + act → gh stub called, intent moved to done.
#   7. FAILS OPEN: direct gh call bypasses the fence (naive executor proves the gap).
#   8. broker.sh shim execs the broker binary.
#   9. SPIRA_BROKER_BIN in conf.sh allowlist.
#  10. read: unknown verb rejected, run-view calls gh with safe JSON fields (no --log),
#      artifact-download calls gh run download with named artifact, missing --artifact fails.
#  11. SPIRA_BROKER_GH_CONFIG_DIR and SPIRA_BROKER_GH_TOKEN in conf.sh allowlist.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control). The check that would fail
# before the fix is run first; only when it finds the offender do later checks mean anything.
#
# covers: broker/* spira/broker.sh spira/build.sh spira/conf.sh
# hermetic-ok: no database required; gh is a stub; czar-fence.sh runs against a fixture env
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s\n' "$1"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1: wanted [$2] in [$3]"; }
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1: did not want [$2] in [$3]"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1: wanted [$2] got [$3]"; }

echo "test-broker.sh"

# Resolve cargo; skip if absent (law-absence-needs-a-positive-control: skip, not vacuous pass).
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-broker: cargo not found — broker binary cannot be built"
    exit 77
fi

BROKER_ROOT="$HERE/../broker"
BROKER_BIN="$BROKER_ROOT/target/release/broker"

# Scratch space: also used if we need to build broker from a writable copy.
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# =========================================================================
echo
echo "POSITIVE CONTROL — broker binary exists and is executable"
# =========================================================================
# If the binary does not exist, build it now. The build writes Cargo.lock next to Cargo.toml;
# in containers the workspace is read-only, so copy the source to a writable tmp path first.
if [ ! -x "$BROKER_BIN" ]; then
    cp -r "$BROKER_ROOT/." "$T/broker-src"
    printf '  (building broker into %s)\n' "$T/broker-target"
    CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/broker-target" \
        "$CARGO_BIN" build --release \
        --manifest-path "$T/broker-src/Cargo.toml" 2>&1 | tail -5
    BROKER_BIN="$T/broker-target/release/broker"
fi

if [ -x "$BROKER_BIN" ]; then
    ok "broker binary is present and executable"
else
    bad "broker binary not found at $BROKER_BIN (positive control: fails before the fix)"
    printf '\n0 passed, 1 failed\n'
    exit 1
fi

# =========================================================================
# SHARED FIXTURE
# =========================================================================

# Minimal git repo for the broker to cd into (repo-map needs a real directory).
REPO_DIR="$T/fake-repo"
git init -q "$REPO_DIR"

# Repo-map: maps label "test-repo" to the fake repo directory.
REPO_MAP="$T/repo-map"
printf 'test-repo | %s | push | origin/main | | \n' "$REPO_DIR" > "$REPO_MAP"

# Stub gh: records what it was called with and exits 0.
# For run view calls it outputs minimal JSON so broker read can return it.
GH_STUB="$T/gh"
cat > "$GH_STUB" << 'ENDGH'
#!/usr/bin/env bash
printf 'stub-gh: %s\n' "$*" >> "$SPIRA_BROKER_GH_LOG"
case "$1 $2" in
    "run view") printf '{"status":"completed","conclusion":"success","databaseId":12345,"name":"CI"}\n' ;;
esac
exit 0
ENDGH
chmod +x "$GH_STUB"
GH_LOG="$T/gh.log"

# Fixture environment used for most tests. SPIRA_RUN is the broker's working directory.
SPIRA_RUN="$T/run"
mkdir -p "$SPIRA_RUN"

base_env() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$T/home" \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="$SPIRA_RUN" \
        SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_BROKER_GH="$GH_STUB" \
        SPIRA_BROKER_GH_LOG="$GH_LOG" \
        SPIRA_BD="true" \
        "$@"
}

# =========================================================================
echo
echo "submit — creates an intent JSON in inbox/"
# =========================================================================
intent_id="$(base_env \
    FAYTH_NAME=czar \
    SPIRA_AEON=test-aeon \
    "$BROKER_BIN" submit run-rerun test-repo/12345 \
        --reason "test reason" --bead sp-test --class deadlock 2>&1)"
rc=$?
is "submit exits 0" "0" "$rc"

INTENT_FILE="$SPIRA_RUN/broker/inbox/${intent_id}.json"
if [ -f "$INTENT_FILE" ]; then
    ok "submit creates intent file at inbox/<id>.json"
    content="$(cat "$INTENT_FILE")"
    want "intent contains verb run-rerun"       '"run-rerun"'  "$content"
    want "intent contains repo test-repo"       '"test-repo"'  "$content"
    want "intent contains number 12345"         '"12345"'      "$content"
    want "intent contains reason"               'test reason'  "$content"
    want "intent contains bead sp-test"         '"sp-test"'    "$content"
    want "intent contains fayth czar"           '"czar"'       "$content"
    want "intent contains aeon test-aeon"       'test-aeon'    "$content"
    want "intent contains class deadlock"       '"deadlock"'   "$content"
    want "intent contains submitted_at"         'submitted_at' "$content"
else
    bad "submit creates intent file at inbox/<id>.json"
fi

# =========================================================================
echo
echo "execute — unknown verb refused"
# =========================================================================
# Plant an intent with an unknown verb in the inbox.
UNKNOWN_FILE="$SPIRA_RUN/broker/inbox/unknown-verb.json"
cat > "$UNKNOWN_FILE" << 'EOUNK'
{"id":"unknown-verb","verb":"run-explode","repo":"test-repo","number":"1","reason":"x","bead":"sp-t","fayth":"czar","aeon":"a","class":"deadlock","submitted_at":1000000000}
EOUNK

base_env \
    SPIRA_CZAR_STAGE_DEADLOCK=act \
    "$BROKER_BIN" execute >/dev/null 2>&1

audit="$SPIRA_RUN/broker/audit.jsonl"
if [ -f "$audit" ]; then
    audit_content="$(cat "$audit")"
    want "unknown verb recorded in audit" '"run-explode"' "$audit_content"
    want "unknown verb outcome is REFUSED"  '"REFUSED"'     "$audit_content"
    want "unknown verb detail names policy" 'policy table'  "$audit_content"
else
    bad "audit.jsonl created after execute"
fi
[ -d "$SPIRA_RUN/broker/refused" ] && [ "$(ls "$SPIRA_RUN/broker/refused/")" ] \
    && ok "unknown-verb intent moved to refused/" \
    || bad "unknown-verb intent moved to refused/"

# =========================================================================
echo
echo "execute — builder fayth + czar-only verb refused"
# =========================================================================
BUILDER_FILE="$SPIRA_RUN/broker/inbox/builder-czar-verb.json"
cat > "$BUILDER_FILE" << 'EOBLD'
{"id":"builder-czar-verb","verb":"run-rerun","repo":"test-repo","number":"99","reason":"x","bead":"sp-t","fayth":"builder","aeon":"a","class":"deadlock","submitted_at":1000000001}
EOBLD

> "$GH_LOG"
base_env \
    SPIRA_CZAR_STAGE_DEADLOCK=act \
    "$BROKER_BIN" execute >/dev/null 2>&1

audit_content="$(cat "$audit" 2>/dev/null || echo "")"
# The builder+czar-verb entry should be REFUSED in the audit.
builder_line="$(grep 'builder-czar-verb' "$audit" 2>/dev/null || echo "")"
want "builder+czar-only verb REFUSED in audit"  '"REFUSED"' "$builder_line"
want "builder+czar-only refusal names fayth"    'builder'   "$builder_line"
# gh must NOT have been called for this intent.
gh_calls="$(cat "$GH_LOG" 2>/dev/null || echo "")"
lack "gh not called for builder+czar-only refusal" "99" "$gh_calls"

# =========================================================================
echo
echo "FAILS OPEN — direct gh call bypasses policy (naive executor)"
# =========================================================================
# A naive executor that calls gh directly without a policy check would succeed.
# This demonstrates WHAT the broker prevents.
> "$GH_LOG"
(cd "$REPO_DIR" && SPIRA_BROKER_GH_LOG="$GH_LOG" "$GH_STUB" run rerun 99)
naive_result="$(cat "$GH_LOG")"
want "naive gh exec succeeds regardless of fayth (FAILS OPEN)" "99" "$naive_result"
ok "proved FAILS OPEN: gh runs with no policy; broker is the guard"

# =========================================================================
echo
echo "execute — czar + run-rerun + shadow → CZAR-WOULD"
# =========================================================================
CZAR_SHADOW_FILE="$SPIRA_RUN/broker/inbox/czar-shadow.json"
cat > "$CZAR_SHADOW_FILE" << 'EOCS'
{"id":"czar-shadow","verb":"run-rerun","repo":"test-repo","number":"555","reason":"rerun test","bead":"sp-shadow","fayth":"czar","aeon":"czar-1","class":"deadlock","submitted_at":1000000002}
EOCS

> "$GH_LOG"
# SPIRA_CZAR_STAGE_DEADLOCK unset → shadow (default)
base_env \
    SPIRA_CZAR_STAGE_DEADLOCK="" \
    "$BROKER_BIN" execute >/dev/null 2>&1

shadow_line="$(grep 'czar-shadow' "$audit" 2>/dev/null || echo "")"
want "czar+shadow run-rerun → CZAR-WOULD in audit" '"CZAR-WOULD"' "$shadow_line"
want "czar+shadow audit detail names class"         'deadlock'     "$shadow_line"
gh_calls="$(cat "$GH_LOG" 2>/dev/null || echo "")"
lack "gh NOT called when shadow" "555" "$gh_calls"

if [ -d "$SPIRA_RUN/broker/refused" ]; then
    shadow_refused="$(ls "$SPIRA_RUN/broker/refused/" 2>/dev/null | grep czar-shadow || echo "")"
    [ -n "$shadow_refused" ] \
        && ok "czar-shadow intent moved to refused/" \
        || bad "czar-shadow intent not in refused/"
fi

# =========================================================================
echo
echo "execute — czar + run-rerun + act → gh stub called"
# =========================================================================
CZAR_ACT_FILE="$SPIRA_RUN/broker/inbox/czar-act.json"
cat > "$CZAR_ACT_FILE" << 'EOCA'
{"id":"czar-act","verb":"run-rerun","repo":"test-repo","number":"777","reason":"act rerun","bead":"sp-act","fayth":"czar","aeon":"czar-2","class":"deadlock","submitted_at":1000000003}
EOCA

> "$GH_LOG"
base_env \
    SPIRA_CZAR_STAGE_DEADLOCK=act \
    "$BROKER_BIN" execute >/dev/null 2>&1

act_line="$(grep 'czar-act' "$audit" 2>/dev/null || echo "")"
want "czar+act run-rerun → DONE in audit"    '"DONE"'    "$act_line"
gh_calls="$(cat "$GH_LOG" 2>/dev/null || echo "")"
want "gh called with run rerun 777 in act"   "run rerun" "$gh_calls"
want "gh called with the run number 777"     "777"       "$gh_calls"

[ -d "$SPIRA_RUN/broker/done" ] && \
    { ls "$SPIRA_RUN/broker/done/" 2>/dev/null | grep -q czar-act \
        && ok "czar-act intent moved to done/" \
        || bad "czar-act intent not in done/"; } \
    || bad "done/ directory created"

# =========================================================================
echo
echo "broker.sh shim — execs the broker binary"
# =========================================================================
SHIM="$HERE/broker.sh"
if [ -f "$SHIM" ] && [ -x "$SHIM" ]; then
    ok "broker.sh is present and executable"
    # Check it is a thin shim: no logic beyond sourcing conf.sh and exec.
    content="$(cat "$SHIM")"
    want "broker.sh execs broker binary"  "exec" "$content"
    want "broker.sh sources conf.sh"      "conf.sh" "$content"
    lack "broker.sh contains no logic"   "case" "$content"
else
    bad "broker.sh is present and executable"
fi

# =========================================================================
echo
echo "SPIRA_BROKER_BIN in conf.sh allowlist"
# =========================================================================
conf_sh="$HERE/conf.sh"
grep -q 'SPIRA_BROKER_BIN' "$conf_sh" 2>/dev/null \
    && ok "SPIRA_BROKER_BIN in conf.sh" \
    || bad "SPIRA_BROKER_BIN missing from conf.sh"

# =========================================================================
echo
echo "build.sh names broker"
# =========================================================================
grep -q 'broker' "$HERE/build.sh" 2>/dev/null \
    && ok "build.sh mentions broker" \
    || bad "build.sh does not mention broker"

# =========================================================================
echo
echo "POSITIVE CONTROL — read: unknown verb rejected before the fix"
# =========================================================================
# If this were run on a build without read support, broker would exit 2 with "usage" —
# but it would NOT name "run-view". This proves the verb table is wired.
read_unknown_out="$(base_env "$BROKER_BIN" read no-such-verb test-repo/999 2>&1)"
read_unknown_rc=$?
[ "$read_unknown_rc" -ne 0 ] \
    && ok "broker read: unknown verb exits non-zero (positive control)" \
    || bad "broker read: unknown verb should exit non-zero"
want "broker read: unknown verb output names run-view" "run-view" "$read_unknown_out"

# =========================================================================
echo
echo "broker read run-view — calls gh with safe JSON fields, no --log"
# =========================================================================
> "$GH_LOG"
read_view_out="$(base_env \
    "$BROKER_BIN" read run-view test-repo/12345 2>&1)"
read_view_rc=$?
is "broker read run-view exits 0" "0" "$read_view_rc"
want "broker read run-view output contains status"     '"status"'     "$read_view_out"
want "broker read run-view output contains conclusion" '"conclusion"' "$read_view_out"
gh_calls="$(cat "$GH_LOG" 2>/dev/null || echo "")"
want "gh called with run view"        "run view"   "$gh_calls"
want "gh called with run id 12345"    "12345"      "$gh_calls"
want "gh called with --json"          "--json"     "$gh_calls"
lack "gh NOT called with --log"       "--log"      "$gh_calls"

# =========================================================================
echo
echo "broker read artifact-download — calls gh run download with named artifact"
# =========================================================================
ART_DIR="$T/artifacts-out"
> "$GH_LOG"
art_out="$(base_env \
    "$BROKER_BIN" read artifact-download test-repo/12345 \
    --artifact batch-results --output-dir "$ART_DIR" 2>&1)"
art_rc=$?
is "broker read artifact-download exits 0" "0" "$art_rc"
want "artifact-download prints the output dir" "$ART_DIR" "$art_out"
gh_calls="$(cat "$GH_LOG" 2>/dev/null || echo "")"
want "gh called with run download"         "run download" "$gh_calls"
want "gh called with -n batch-results"     "batch-results" "$gh_calls"
want "gh called with -D and output dir"    "$ART_DIR"      "$gh_calls"
[ -d "$ART_DIR" ] \
    && ok "artifact-download creates output directory" \
    || bad "artifact-download should create output directory"

# =========================================================================
echo
echo "broker read artifact-download — --artifact required"
# =========================================================================
art_missing_out="$(base_env \
    "$BROKER_BIN" read artifact-download test-repo/12345 2>&1)"
art_missing_rc=$?
[ "$art_missing_rc" -ne 0 ] \
    && ok "artifact-download without --artifact exits non-zero" \
    || bad "artifact-download without --artifact should fail"
want "error names --artifact" "--artifact" "$art_missing_out"

# =========================================================================
echo
echo "SPIRA_BROKER_GH_CONFIG_DIR and SPIRA_BROKER_GH_TOKEN in conf.sh allowlist"
# =========================================================================
grep -q 'SPIRA_BROKER_GH_CONFIG_DIR' "$conf_sh" 2>/dev/null \
    && ok "SPIRA_BROKER_GH_CONFIG_DIR in conf.sh" \
    || bad "SPIRA_BROKER_GH_CONFIG_DIR missing from conf.sh"
grep -q 'SPIRA_BROKER_GH_TOKEN' "$conf_sh" 2>/dev/null \
    && ok "SPIRA_BROKER_GH_TOKEN in conf.sh" \
    || bad "SPIRA_BROKER_GH_TOKEN missing from conf.sh"

# =========================================================================
echo
echo "POSITIVE CONTROL — broker token: subcommand exists (fails without creds, not with 'usage')"
# =========================================================================
# If the token subcommand is not wired, broker exits 2 with the usage message.
# When it IS wired but no credentials are configured, it exits non-zero with
# a message naming the missing credentials — not the generic usage banner.
token_out="$(base_env \
    HOME="$T/home" \
    "$BROKER_BIN" token 2>&1)"
token_rc=$?
[ "$token_rc" -ne 0 ] \
    && ok "broker token exits non-zero when no credentials configured" \
    || bad "broker token should exit non-zero with no credentials (positive control)"
want "broker token error names credentials" "credentials" "$token_out"
lack "broker token not a usage line" "broker execute" "$token_out"

# =========================================================================
echo
echo "broker token: SPIRA_BROKER_GH_TOKEN static fallback via gh_env"
# =========================================================================
# When SPIRA_BROKER_GH_TOKEN is set and no App creds exist, broker execute
# should pass GH_TOKEN=<that value> to gh. Verify via the audit that the
# execute path ran (gh stub is called) — the env propagation is tested in
# execute.rs unit path; here we confirm the integration does not regress.
STATIC_FILE="$SPIRA_RUN/broker/inbox/static-token-test.json"
cat > "$STATIC_FILE" << 'EOST'
{"id":"static-token-test","verb":"pr-comment","repo":"test-repo","number":"88","reason":"static token test","bead":"sp-tok","fayth":"builder","aeon":"a","class":"","submitted_at":1000000010}
EOST

> "$GH_LOG"
base_env \
    SPIRA_BROKER_GH_TOKEN="test-static-tok" \
    HOME="$T/home" \
    "$BROKER_BIN" execute >/dev/null 2>&1

tok_line="$(grep 'static-token-test' "$audit" 2>/dev/null || echo "")"
want "static fallback: pr-comment via builder fayth → DONE" '"DONE"' "$tok_line"

# =========================================================================
echo
echo "SPIRA_GH and SPIRA_GH_APP_CONFIG in conf.sh allowlist"
# =========================================================================
grep -q 'SPIRA_GH\b' "$conf_sh" 2>/dev/null \
    && ok "SPIRA_GH in conf.sh" \
    || bad "SPIRA_GH missing from conf.sh"
grep -q 'SPIRA_GH_APP_CONFIG' "$conf_sh" 2>/dev/null \
    && ok "SPIRA_GH_APP_CONFIG in conf.sh" \
    || bad "SPIRA_GH_APP_CONFIG missing from conf.sh"

# =========================================================================
echo
echo "gh-app.sh: present and executable"
# =========================================================================
GH_APP_SH="$HERE/gh-app.sh"
[ -f "$GH_APP_SH" ] && [ -x "$GH_APP_SH" ] \
    && ok "gh-app.sh is present and executable" \
    || bad "gh-app.sh is not present or not executable"

# =========================================================================
echo
echo "app-token.sh: present, executable, and names openssl+curl"
# =========================================================================
APP_TOK_SH="$HERE/app-token.sh"
if [ -f "$APP_TOK_SH" ] && [ -x "$APP_TOK_SH" ]; then
    ok "app-token.sh is present and executable"
    _atcontent="$(cat "$APP_TOK_SH")"
    want "app-token.sh uses openssl" "openssl"  "$_atcontent"
    want "app-token.sh uses curl"    "curl"      "$_atcontent"
else
    bad "app-token.sh is not present or not executable"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
