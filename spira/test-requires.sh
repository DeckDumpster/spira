#!/usr/bin/env bash
#
# test-requires.sh — # requires: declared preconditions become skips with a reason.
#
# WHAT THIS PROVES
#   A. Parser: suite_requires_of extracts tokens from a # requires: line.
#      No declaration returns empty.  Commas are treated as delimiters.
#
#   B. Runner integration (container): a suite declaring # requires: <token>
#      where <token> is not on the container's PATH is recorded as skip-req
#      with the token named in the fingerprint field — not as red, and not as
#      the exit-77 self-skip.
#
#   C. Positive control: a suite declaring # requires: bash (always met) runs
#      normally and records ok, not skip-req.  Without this, a requirements
#      check that always skips would look identical to one that works.
#
#   D. Distinction from exit-77 skip: a suite that self-skips (exit 77) records
#      status "skip"; a suite that is pre-empted for an unmet requirement records
#      status "skip-req".  The two are counted separately.
#
# SEEN TO FAIL AGAINST UNFIXED TREE (law-a-regression-test-must-be-seen-to-fail):
#   Against the tree before suite_requires_of was added and testenv-batch.sh was
#   patched:
#     Part A: "suite_requires_of is defined" — FAIL: function not found
#     Part B-D: fixture suite with # requires: spira-nonexistent-xyz ran normally
#               inside the container and exited 0; result status was "ok" not
#               "skip-req". Assertion "skip-req status" failed: got "ok".
#
# host-reason: Part A tests the parser on the host.
#              Part B-D requires podman for container integration.
# tier: T0
# covers: spira/suite-covers.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

isfile()  { [ -f "$2" ] && ok "$1" || bad "$1" "file not found: $2"; }

find_results_dir() {
    find "$1" -maxdepth 2 -name batch.meta 2>/dev/null | head -1 | xargs dirname 2>/dev/null || true
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-requires.sh"

# ===========================================================================
# PART A: PARSER — no container required.
# ===========================================================================
echo
echo "Part A: suite_requires_of parser (no container)"

# shellcheck disable=SC1090
. "$HERE/suite-covers.sh"

if declare -f suite_requires_of >/dev/null 2>&1; then
    ok "A0: suite_requires_of is defined in suite-covers.sh"
else
    bad "A0: suite_requires_of is defined in suite-covers.sh" "function not found"
    printf 'SKIP test-requires.sh Part A: parser not implemented\n' >&2
    # Continue to Part B so its failures are also recorded.
fi

# A1: a suite with a # requires: line returns the tokens.
cat > "$TMP/fx-has-req.sh" <<'EOF'
#!/usr/bin/env bash
# requires: spira-nonexistent-xyz, bash
exit 0
EOF

if declare -f suite_requires_of >/dev/null 2>&1; then
    _reqs_a1="$(suite_requires_of "$TMP/fx-has-req.sh")"
    want "A1: # requires: returns the tokens" "spira-nonexistent-xyz" "$_reqs_a1"
    want "A1: both tokens returned"           "bash"                  "$_reqs_a1"
else
    bad "A1: # requires: returns the tokens"   "suite_requires_of not defined"
    bad "A1: both tokens returned"             "suite_requires_of not defined"
fi

# A2: a suite with no # requires: line returns empty.
cat > "$TMP/fx-no-req.sh" <<'EOF'
#!/usr/bin/env bash
# covers: some/file.sh
exit 0
EOF
if declare -f suite_requires_of >/dev/null 2>&1; then
    _reqs_a2="$(suite_requires_of "$TMP/fx-no-req.sh")"
    is "A2: no # requires: line returns empty" "" "$_reqs_a2"
else
    bad "A2: no # requires: line returns empty" "suite_requires_of not defined"
fi

# A3: commas are stripped — "claude, bd" yields two tokens, not one.
cat > "$TMP/fx-comma-req.sh" <<'EOF'
#!/usr/bin/env bash
# requires: claude, bd
exit 0
EOF
if declare -f suite_requires_of >/dev/null 2>&1; then
    _reqs_a3="$(suite_requires_of "$TMP/fx-comma-req.sh")"
    want "A3: comma-separated tokens parsed" "claude" "$_reqs_a3"
    want "A3: second comma-token present"    "bd"     "$_reqs_a3"
    nowant "A3: comma not present as literal char" "," "$_reqs_a3"
else
    bad "A3: comma-separated tokens parsed" "suite_requires_of not defined"
    bad "A3: second comma-token present"    "suite_requires_of not defined"
    bad "A3: comma not present as literal char" "suite_requires_of not defined"
fi

# ===========================================================================
# PART B-D (runner integration) RETIRED with spira/testenv-batch.sh: the Rust runner's
# requirement check is `cargo test -p testenv` —
# requirements_are_checked_once_each_and_testenv_is_always_met, disabled_and_skip_req_render_a_dash_rc.
# ===========================================================================
echo
tl_summary
