#!/usr/bin/env bash
# ci-config.sh <checkout> — give every later step of a hosted-runner job a resolvable
# SPIRA_TOML: the checkout's complete fixture plus an override layer for the keys the
# fixture leaves empty. A runner has no box config, and a release binary refuses without one.
set -euo pipefail
checkout="${1:?usage: ci-config.sh <checkout>}"
fixture="$checkout/spira-config/tests/fixtures/complete.toml"
layered="${RUNNER_TEMP:?}/ci-config.toml"
owner="$(printf '%s' "${GITHUB_REPOSITORY_OWNER:?}" | tr '[:upper:]' '[:lower:]')"

sed "s|^testenv_registry = \"\"\$|testenv_registry = \"ghcr.io/$owner\"|" "$fixture" > "$layered"
grep -q "^testenv_registry = \"ghcr.io/$owner\"\$" "$layered" || {
  printf 'ci-config: the fixture has no empty testenv_registry to layer over\n' >&2
  exit 1
}
printf 'SPIRA_TOML=%s\n' "$layered" >> "$GITHUB_ENV"
