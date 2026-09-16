#!/usr/bin/env bash
# covers: spira/suite-state.sh spira/suite-state spira/suites.sh spira/testenv-batch.sh
# Suite lifecycle state: parse, lint, write, activate, and transition commands.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"
. "$HERE/suite-state.sh"

pass() { printf 'ok: %s\n' "$*"; _ok=$((_ok+1)); }
fail() { printf 'FAIL: %s\n' "$*" >&2; _fail=$((_fail+1)); }
_ok=0; _fail=0

# ---------------------------------------------------------------------------
# parse — basic round-trip and field extraction
# ---------------------------------------------------------------------------
_f="$(mktemp)"

# An empty file yields no output.
suite_state_parse "$_f" > /dev/null && pass "parse: empty file yields no output" \
    || fail "parse: empty file should not error"

# A comment-only file yields no output.
printf '# comment\n' > "$_f"
_n="$(suite_state_parse "$_f" | wc -l | tr -d ' ')"
[ "$_n" = 0 ] && pass "parse: comment-only yields no lines" \
    || fail "parse: comment-only should yield 0 lines, got $_n"

# A well-formed quarantined line is parsed.
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | sp-abc | flaky dns\n' > "$_f"
_line="$(suite_state_parse "$_f")"
_suite="$(printf '%s' "$_line" | cut -f1)"
_state="$(printf '%s' "$_line" | cut -f2)"
[ "$_suite" = "test-foo.sh" ] && pass "parse: suite field" || fail "parse: suite field got '$_suite'"
[ "$_state" = "quarantined" ] && pass "parse: state field" || fail "parse: state field got '$_state'"

# A well-formed disabled line is parsed.
printf 'test-bar.sh | disabled | 2026-01-01T00:00:00Z | | too slow\n' > "$_f"
_state="$(suite_state_parse "$_f" | cut -f2)"
[ "$_state" = "disabled" ] && pass "parse: disabled state" || fail "parse: disabled state got '$_state'"

# Unknown state is silently skipped (fail-closed: treated as active).
printf 'test-baz.sh | bogus | 2026-01-01T00:00:00Z | | reason\n' > "$_f"
_n="$(suite_state_parse "$_f" | wc -l | tr -d ' ')"
[ "$_n" = 0 ] && pass "parse: unknown state silently skipped" \
    || fail "parse: unknown state should be skipped, got $_n lines"

# Unparseable line (missing pipes) is silently skipped.
printf 'test-nopipes.sh\n' > "$_f"
_n="$(suite_state_parse "$_f" | wc -l | tr -d ' ')"
[ "$_n" = 0 ] && pass "parse: unparseable line silently skipped" \
    || fail "parse: unparseable line should be skipped, got $_n lines"

# ---------------------------------------------------------------------------
# suite_state_of — returns active for absent suite, correct state for present
# ---------------------------------------------------------------------------
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | sp-abc | reason\n' > "$_f"
_st="$(suite_state_of "$_f" "test-foo.sh")"
[ "$_st" = "quarantined" ] && pass "state_of: quarantined" || fail "state_of: expected quarantined, got '$_st'"

_st="$(suite_state_of "$_f" "test-nothere.sh")"
[ "$_st" = "active" ] && pass "state_of: absent suite is active" || fail "state_of: expected active, got '$_st'"

# ---------------------------------------------------------------------------
# suite_state_write / suite_state_clear — round-trip
# ---------------------------------------------------------------------------
: > "$_f"
suite_state_write "$_f" "test-x.sh" quarantined "sp-xyz" "reason for x"
_st="$(suite_state_of "$_f" "test-x.sh")"
[ "$_st" = "quarantined" ] && pass "write: quarantined state stored" || fail "write: expected quarantined, got '$_st'"

# Writing a second time replaces the entry (no duplicates).
suite_state_write "$_f" "test-x.sh" disabled "" "now disabled"
_n="$(grep -c 'test-x.sh' "$_f" 2>/dev/null || echo 0)"
[ "$_n" = 1 ] && pass "write: second write replaces entry" || fail "write: expected 1 entry, got $_n"
_st="$(suite_state_of "$_f" "test-x.sh")"
[ "$_st" = "disabled" ] && pass "write: updated state is disabled" || fail "write: expected disabled, got '$_st'"

# Clear removes the entry (activate round-trip).
suite_state_clear "$_f" "test-x.sh"
_st="$(suite_state_of "$_f" "test-x.sh")"
[ "$_st" = "active" ] && pass "clear: suite returns to active" || fail "clear: expected active, got '$_st'"
if grep -q 'test-x.sh' "$_f" 2>/dev/null; then
    fail "clear: entry still in file"
else
    pass "clear: no entry remains in file"
fi

# Writing active state does not write a line.
suite_state_write "$_f" "test-y.sh" active "" ""
if grep -q 'test-y.sh' "$_f" 2>/dev/null; then
    fail "write: active state left an entry"
else
    pass "write: active state writes no entry"
fi

# ---------------------------------------------------------------------------
# quarantine + activate round-trip does not disturb other entries
# ---------------------------------------------------------------------------
: > "$_f"
printf 'test-other.sh | disabled | 2026-01-01T00:00:00Z | | other reason\n' >> "$_f"
suite_state_write "$_f" "test-x.sh" quarantined "sp-abc" "flaky"
suite_state_clear "$_f" "test-x.sh"
_other_lines="$(grep 'test-other.sh' "$_f" | wc -l | tr -d ' ')"
[ "$_other_lines" = 1 ] && pass "round-trip: other entry untouched" \
    || fail "round-trip: expected other entry to survive, got $_other_lines"
_x_lines="$(grep 'test-x.sh' "$_f" | wc -l | tr -d ' ')"
[ "$_x_lines" = 0 ] && pass "round-trip: cleared entry is gone" \
    || fail "round-trip: cleared entry still present"

# ---------------------------------------------------------------------------
# lint — missing reason, bead-less quarantine, unknown state, unparseable
# ---------------------------------------------------------------------------
: > "$_f"
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | sp-abc | valid reason\n' > "$_f"
suite_state_lint "$_f" && pass "lint: valid quarantine passes" \
    || fail "lint: valid quarantine should pass"

# Missing reason.
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | sp-abc |\n' > "$_f"
suite_state_lint "$_f" 2>/dev/null && fail "lint: missing reason should fail" \
    || pass "lint: missing reason fails"

# Quarantine with no bead.
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | | reason\n' > "$_f"
suite_state_lint "$_f" 2>/dev/null && fail "lint: bead-less quarantine should fail" \
    || pass "lint: bead-less quarantine fails"

# Unknown state in lint (reported as error, not silently skipped).
printf 'test-foo.sh | bogus | 2026-01-01T00:00:00Z | | reason\n' > "$_f"
suite_state_lint "$_f" 2>/dev/null && fail "lint: unknown state should fail" \
    || pass "lint: unknown state fails"

# Missing suite (when suite-dir given).
printf 'no-such-suite-xyz.sh | disabled | 2026-01-01T00:00:00Z | | reason\n' > "$_f"
suite_state_lint "$_f" "$HERE" 2>/dev/null && fail "lint: missing suite should fail" \
    || pass "lint: missing suite fails"

# ---------------------------------------------------------------------------
# fail-closed: unknown state and unparseable line both block (suite_state_of)
# ---------------------------------------------------------------------------
# Unknown state in state file is treated as active (blocking = not quarantined/disabled).
printf 'test-foo.sh | bogus | 2026-01-01T00:00:00Z | | reason\n' > "$_f"
_st="$(suite_state_of "$_f" "test-foo.sh")"
[ "$_st" = "active" ] && pass "fail-closed: unknown state treated as active" \
    || fail "fail-closed: unknown state should be active, got '$_st'"

# Unparseable line → suite treated as active.
printf 'test-foo.sh unparseable\n' > "$_f"
_st="$(suite_state_of "$_f" "test-foo.sh")"
[ "$_st" = "active" ] && pass "fail-closed: unparseable line treated as active" \
    || fail "fail-closed: unparseable line should be active, got '$_st'"

rm -f "$_f"

# ---------------------------------------------------------------------------
# fixture-tree reading: git show reads state from the candidate branch,
# not the working tree. This is the mechanism testenv-batch.sh uses.
# We replicate it here to prove the read path works correctly.
# ---------------------------------------------------------------------------
_gdir="$(mktemp -d)"
(
    cd "$_gdir"
    git init -q
    git config user.email "test@example.com"
    git config user.name "Test"
    mkdir -p spira
    printf 'test-canary.sh | quarantined | 2026-01-01T00:00:00Z | sp-t | fixture\n' \
        > spira/suite-state
    git add spira/suite-state
    git commit -q -m "fixture: add quarantine entry"
    _head="$(git rev-parse HEAD)"
    # Read via git show (the same path testenv-batch uses).
    _tmp="$(mktemp)"
    git show "${_head}:spira/suite-state" > "$_tmp" 2>/dev/null
    _st="$(suite_state_of "$_tmp" "test-canary.sh")"
    rm -f "$_tmp"
    [ "$_st" = "quarantined" ] && printf 'PASS\n' || printf 'FAIL: expected quarantined, got %s\n' "$_st"
)
_result="$(cat "$_gdir/result" 2>/dev/null || true)"
# The subshell can't write to variables; check its stdout instead.
_gout="$(cd "$_gdir" && \
    _tmp="$(mktemp)" && \
    git show "HEAD:spira/suite-state" > "$_tmp" 2>/dev/null && \
    suite_state_of "$_tmp" "test-canary.sh" && \
    rm -f "$_tmp")" 2>/dev/null || true
[ "$_gout" = "quarantined" ] && pass "git-show path: fixture tree quarantine is honoured" \
    || fail "git-show path: expected quarantined from git-show read, got '$_gout'"
rm -rf "$_gdir"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
printf '\nASSERTIONS %s\n' "$((_ok + _fail))"
[ "$_fail" -eq 0 ] || { printf 'FAILURES: %s\n' "$_fail"; exit 1; }
