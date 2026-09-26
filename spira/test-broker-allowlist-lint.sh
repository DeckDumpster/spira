#!/usr/bin/env bash
#
# test-broker-allowlist-lint.sh — broker-allowlist-lint.sh catches a dropped declaration.
#
# THE POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control). A conf.sh missing
# one of the broker's env vars is planted, the lint is required to name it, and only then
# is the shipped tree's clean pass evidence of anything.
#
# covers: spira/broker-allowlist-lint.sh spira/conf.sh spira/build.sh spira/broker.sh
# tier: T0
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-broker-allowlist-lint.sh"

LINT="$HERE/broker-allowlist-lint.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# POSITIVE CONTROL — a conf.sh with one name missing is caught by --scan-conf.
# ---------------------------------------------------------------------------
printf 'SPIRA_BROKER_BIN=x\nSPIRA_BROKER_GH_CONFIG_DIR=x\nSPIRA_GH=x\nSPIRA_GH_APP_CONFIG=x\n' \
    > "$TMP/conf-missing-one.sh"
out="$(bash "$LINT" --scan-conf "$TMP/conf-missing-one.sh")"
is "positive control: names the one missing var" "SPIRA_BROKER_GH_TOKEN" "$out"

# The escape: adding the name silences it.
printf 'SPIRA_BROKER_BIN=x\nSPIRA_BROKER_GH_CONFIG_DIR=x\nSPIRA_BROKER_GH_TOKEN=x\nSPIRA_GH=x\nSPIRA_GH_APP_CONFIG=x\n' \
    > "$TMP/conf-complete.sh"
is "a complete conf.sh reports nothing missing" "" "$(bash "$LINT" --scan-conf "$TMP/conf-complete.sh")"

# ---------------------------------------------------------------------------
# build.sh --scan-build
# ---------------------------------------------------------------------------
printf 'echo hello\n' > "$TMP/build-no-broker.sh"
is "build.sh with no mention of broker is flagged" "broker" "$(bash "$LINT" --scan-build "$TMP/build-no-broker.sh")"
printf 'printf broker: %%s\\n "$SPIRA_BROKER_BIN"\n' > "$TMP/build-with-broker.sh"
is "build.sh mentioning broker reports nothing" "" "$(bash "$LINT" --scan-build "$TMP/build-with-broker.sh")"

# ---------------------------------------------------------------------------
# broker.sh shim --scan-shim
# ---------------------------------------------------------------------------
printf '#!/usr/bin/env bash\n. "$(dirname "$0")/conf.sh"\nexec "$SPIRA_BROKER_BIN" "$@"\n' > "$TMP/shim-good.sh"
is "a thin shim reports nothing missing" "" "$(bash "$LINT" --scan-shim "$TMP/shim-good.sh")"

printf '#!/usr/bin/env bash\nexec /bin/true\n' > "$TMP/shim-no-conf.sh"
want "a shim with no conf.sh source is flagged" "sources conf.sh" "$(bash "$LINT" --scan-shim "$TMP/shim-no-conf.sh")"

printf '#!/usr/bin/env bash\n. conf.sh\ncase "$1" in\n  x) echo x ;;\nesac\n' > "$TMP/shim-with-logic.sh"
want "a shim with a case statement is flagged as carrying logic" "case statement" "$(bash "$LINT" --scan-shim "$TMP/shim-with-logic.sh")"

# ---------------------------------------------------------------------------
# THE SHIPPED TREE. Read through the control above, a clean run now means something.
# ---------------------------------------------------------------------------
out="$(bash "$LINT")"; rc=$?
is "the shipped tree passes" "0" "$rc"
want "and says clean" "clean" "$out"

tl_summary
