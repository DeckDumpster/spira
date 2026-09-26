#!/usr/bin/env bash
# tier: T1
# covers: spira/suite-state.sh spira/suite-state spira/suite-state-fence.sh spira/suites.sh spira/testenv-batch.sh UC-safety-fences-28
#
# Suite lifecycle state: parse, lint, write, activate, and transition commands (D4/UC-28:
# these structural rows already covered the no-bead, missing-suite and missing-reason
# checks that suite-state-fence.sh re-ran against a real Dolt DB; that duplicate suite is
# now gone, and its one distinct rule — a quarantine against a CLOSED bead has no exit
# path — lives below as a single row against a stub `bd`, demoted from T2 to T1).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/lib.sh"
. "$HERE/suite-state.sh"

# ---------------------------------------------------------------------------
# parse — basic round-trip and field extraction
# ---------------------------------------------------------------------------
_f="$(mktemp)"

# An empty file yields no output.
suite_state_parse "$_f" > /dev/null && ok "parse: empty file yields no output" \
    || bad "parse: empty file should not error"

# A comment-only file yields no output.
printf '# comment\n' > "$_f"
_n="$(suite_state_parse "$_f" | wc -l | tr -d ' ')"
[ "$_n" = 0 ] && ok "parse: comment-only yields no lines" \
    || bad "parse: comment-only should yield 0 lines, got $_n"

# A well-formed quarantined line is parsed.
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | sp-abc | flaky dns\n' > "$_f"
_line="$(suite_state_parse "$_f")"
_suite="$(printf '%s' "$_line" | cut -f1)"
_state="$(printf '%s' "$_line" | cut -f2)"
[ "$_suite" = "test-foo.sh" ] && ok "parse: suite field" || bad "parse: suite field got '$_suite'"
[ "$_state" = "quarantined" ] && ok "parse: state field" || bad "parse: state field got '$_state'"

# A well-formed disabled line is parsed.
printf 'test-bar.sh | disabled | 2026-01-01T00:00:00Z | | too slow\n' > "$_f"
_state="$(suite_state_parse "$_f" | cut -f2)"
[ "$_state" = "disabled" ] && ok "parse: disabled state" || bad "parse: disabled state got '$_state'"

# Unknown state is silently skipped (fail-closed: treated as active).
printf 'test-baz.sh | bogus | 2026-01-01T00:00:00Z | | reason\n' > "$_f"
_n="$(suite_state_parse "$_f" | wc -l | tr -d ' ')"
[ "$_n" = 0 ] && ok "parse: unknown state silently skipped" \
    || bad "parse: unknown state should be skipped, got $_n lines"

# Unparseable line (missing pipes) is silently skipped.
printf 'test-nopipes.sh\n' > "$_f"
_n="$(suite_state_parse "$_f" | wc -l | tr -d ' ')"
[ "$_n" = 0 ] && ok "parse: unparseable line silently skipped" \
    || bad "parse: unparseable line should be skipped, got $_n lines"

# ---------------------------------------------------------------------------
# suite_state_of — returns active for absent suite, correct state for present
# ---------------------------------------------------------------------------
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | sp-abc | reason\n' > "$_f"
_st="$(suite_state_of "$_f" "test-foo.sh")"
[ "$_st" = "quarantined" ] && ok "state_of: quarantined" || bad "state_of: expected quarantined, got '$_st'"

_st="$(suite_state_of "$_f" "test-nothere.sh")"
[ "$_st" = "active" ] && ok "state_of: absent suite is active" || bad "state_of: expected active, got '$_st'"

# ---------------------------------------------------------------------------
# suite_state_write / suite_state_clear — round-trip
# ---------------------------------------------------------------------------
: > "$_f"
suite_state_write "$_f" "test-x.sh" quarantined "sp-xyz" "reason for x"
_st="$(suite_state_of "$_f" "test-x.sh")"
[ "$_st" = "quarantined" ] && ok "write: quarantined state stored" || bad "write: expected quarantined, got '$_st'"

# Writing a second time replaces the entry (no duplicates).
suite_state_write "$_f" "test-x.sh" disabled "" "now disabled"
_n="$(grep -c 'test-x.sh' "$_f" 2>/dev/null || echo 0)"
[ "$_n" = 1 ] && ok "write: second write replaces entry" || bad "write: expected 1 entry, got $_n"
_st="$(suite_state_of "$_f" "test-x.sh")"
[ "$_st" = "disabled" ] && ok "write: updated state is disabled" || bad "write: expected disabled, got '$_st'"

# Clear removes the entry (activate round-trip).
suite_state_clear "$_f" "test-x.sh"
_st="$(suite_state_of "$_f" "test-x.sh")"
[ "$_st" = "active" ] && ok "clear: suite returns to active" || bad "clear: expected active, got '$_st'"
if grep -q 'test-x.sh' "$_f" 2>/dev/null; then
    bad "clear: entry still in file" ""
else
    ok "clear: no entry remains in file"
fi

# Writing active state does not write a line.
suite_state_write "$_f" "test-y.sh" active "" ""
if grep -q 'test-y.sh' "$_f" 2>/dev/null; then
    bad "write: active state left an entry" ""
else
    ok "write: active state writes no entry"
fi

# ---------------------------------------------------------------------------
# quarantine + activate round-trip does not disturb other entries
# ---------------------------------------------------------------------------
: > "$_f"
printf 'test-other.sh | disabled | 2026-01-01T00:00:00Z | | other reason\n' >> "$_f"
suite_state_write "$_f" "test-x.sh" quarantined "sp-abc" "flaky"
suite_state_clear "$_f" "test-x.sh"
_other_lines="$(grep 'test-other.sh' "$_f" | wc -l | tr -d ' ')"
[ "$_other_lines" = 1 ] && ok "round-trip: other entry untouched" \
    || bad "round-trip: expected other entry to survive, got $_other_lines"
_x_lines="$(grep 'test-x.sh' "$_f" | wc -l | tr -d ' ')"
[ "$_x_lines" = 0 ] && ok "round-trip: cleared entry is gone" \
    || bad "round-trip: cleared entry still present"

# ---------------------------------------------------------------------------
# lint — missing reason, bead-less quarantine, unknown state, unparseable
# ---------------------------------------------------------------------------
: > "$_f"
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | sp-abc | valid reason\n' > "$_f"
suite_state_lint "$_f" && ok "lint: valid quarantine passes" \
    || bad "lint: valid quarantine should pass"

# Missing reason.
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | sp-abc |\n' > "$_f"
suite_state_lint "$_f" 2>/dev/null && bad "lint: missing reason should fail" "" \
    || ok "lint: missing reason fails"

# Quarantine with no bead.
printf 'test-foo.sh | quarantined | 2026-01-01T00:00:00Z | | reason\n' > "$_f"
suite_state_lint "$_f" 2>/dev/null && bad "lint: bead-less quarantine should fail" "" \
    || ok "lint: bead-less quarantine fails"

# Unknown state in lint (reported as error, not silently skipped).
printf 'test-foo.sh | bogus | 2026-01-01T00:00:00Z | | reason\n' > "$_f"
suite_state_lint "$_f" 2>/dev/null && bad "lint: unknown state should fail" "" \
    || ok "lint: unknown state fails"

# Missing suite (when suite-dir given).
printf 'no-such-suite-xyz.sh | disabled | 2026-01-01T00:00:00Z | | reason\n' > "$_f"
suite_state_lint "$_f" "$HERE" 2>/dev/null && bad "lint: missing suite should fail" "" \
    || ok "lint: missing suite fails"

# ---------------------------------------------------------------------------
# fail-closed: unknown state and unparseable line both block (suite_state_of)
# ---------------------------------------------------------------------------
# Unknown state in state file is treated as active (blocking = not quarantined/disabled).
printf 'test-foo.sh | bogus | 2026-01-01T00:00:00Z | | reason\n' > "$_f"
_st="$(suite_state_of "$_f" "test-foo.sh")"
[ "$_st" = "active" ] && ok "fail-closed: unknown state treated as active" \
    || bad "fail-closed: unknown state should be active, got '$_st'"

# Unparseable line → suite treated as active.
printf 'test-foo.sh unparseable\n' > "$_f"
_st="$(suite_state_of "$_f" "test-foo.sh")"
[ "$_st" = "active" ] && ok "fail-closed: unparseable line treated as active" \
    || bad "fail-closed: unparseable line should be active, got '$_st'"

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
)
_gout="$(cd "$_gdir" && \
    _tmp="$(mktemp)" && \
    git show "HEAD:spira/suite-state" > "$_tmp" 2>/dev/null && \
    suite_state_of "$_tmp" "test-canary.sh" && \
    rm -f "$_tmp")" 2>/dev/null || true
[ "$_gout" = "quarantined" ] && ok "git-show path: fixture tree quarantine is honoured" \
    || bad "git-show path: expected quarantined from git-show read, got '$_gout'"
rm -rf "$_gdir"

# ---------------------------------------------------------------------------
# CLOSED-BEAD QUARANTINE (D4/UC-safety-fences-28, demoted from T2 to T1). A quarantine
# against a CLOSED bead has no exit path: suites.sh hygiene requires land_state:LANDED to
# reactivate one, and CLOSED is never LANDED. suite-state-fence.sh owns the refusal, and
# this is the one row that exercises it — against a stub `bd show --json` rather than a
# real create/close round-trip on a throwaway Dolt DB, which the two structural suites
# above already prove reads shipped correctly (test-bead-lint.sh pins the real `show
# --json` shape). What is under test here is the fence's own branch on that shape, which
# a stub answers exactly as well.
# ---------------------------------------------------------------------------
_sd="$(mktemp -d)"
mkdir -p "$_sd/spira/bin"
cp "$HERE/suite-state-fence.sh" "$HERE/suite-state.sh" "$_sd/spira/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$_sd/spira/test-fake-suite.sh"
printf '#!/usr/bin/env bash\nprintf %%s '"'"'{"status":"closed"}'"'"'\n' > "$_sd/spira/bin/bd"
chmod +x "$_sd/spira/bin/bd"
printf 'test-fake-suite.sh | quarantined | 2026-01-01T00:00:00Z | sp-closed-fixture | flaky\n' \
    > "$_sd/spira/suite-state"
_cb_out="$(SPIRA_BD="$_sd/spira/bin/bd" SPIRA_DB=stub SPIRA_CONF=/dev/null \
    bash "$_sd/spira/suite-state-fence.sh" 2>&1)"; _cb_rc=$?
wantrc "closed-bead quarantine: fence exits 1 (stub bd)" 1 "$_cb_rc"
want   "it names the CLOSED bead"                        "CLOSED" "$_cb_out"
rm -rf "$_sd"

tl_summary
