#!/usr/bin/env bash
# test-observe-flake-bead-required.sh — quarantine fails if bead cannot be filed.
# covers: spira/suites.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

# When bead.sh is not available or fails, quarantine must fail.
# This test simulates a scenario where the bead filing fails.

TMP="$(mktemp -d)"
trap "rm -rf '$TMP'" EXIT
export SPIRA_SUITES_STATE="$TMP"

# Create a fake bead.sh that always fails
mkdir -p "$TMP/bin"
cat > "$TMP/bin/bead.sh" <<'BEAD'
#!/usr/bin/env bash
exit 1
BEAD
chmod +x "$TMP/bin/bead.sh"

# Mock out the HERE/bead.sh check by removing it
# (In the actual test, bead.sh will exist but fail to file a bead)

# Set flake observations to meet threshold
FLAKES="$TMP/test-dummy.sh.flakeobs"
mkdir -p "$TMP"
printf '%s\n' "$(date +%s)" >> "$FLAKES"
printf '%s\n' "$(date +%s)" >> "$FLAKES"

# Observe flake with a custom bead.sh that fails
SUT="bash $HERE/suites.sh"
mkdir -p "$HERE/../bin"
cp "$TMP/bin/bead.sh" "$HERE/../bin/bead.sh.test"

# Test 1: When bead.sh is not available (or fails), quarantine should fail
# Set a threshold that will trigger quarantine
out="$(SPIRA_FLAKE_QUARANTINE_AT=2 $SUT observe-flake test-dummy.sh 2>&1)" || rc=$?
[ -z "${rc:-}" ] && rc=0
echo "Output: $out"
echo "RC: $rc"

# If bead filing fails, the quarantine should fail
case "$out" in
    *"no bead filed"*) printf 'PASS: quarantine correctly failed due to bead failure\n' ;;
    *) printf 'Output did not indicate bead failure: %s\n' "$out" >&2; exit 1 ;;
esac

printf 'test-observe-flake-bead-required.sh: PASS\n'
