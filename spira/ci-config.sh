#!/usr/bin/env bash
# ci-config.sh <checkout> — write the resolvable config a hosted-runner step names in its own
# env as SPIRA_TOML=${{ runner.temp }}/ci-config.toml: the checkout's complete fixture with the
# keys it leaves empty filled in. Never exported job-wide: suites build their own config.
set -euo pipefail
checkout="${1:?usage: ci-config.sh <checkout>}"
fixture="$checkout/spira-config/tests/fixtures/complete.toml"
layered="${RUNNER_TEMP:?}/ci-config.toml"
owner="$(printf '%s' "${GITHUB_REPOSITORY:?}" | cut -d/ -f1 | tr '[:upper:]' '[:lower:]')"

sed "s|^testenv_registry = \"\"\$|testenv_registry = \"ghcr.io/$owner\"|" "$fixture" > "$layered"
grep -q "^testenv_registry = \"ghcr.io/$owner\"\$" "$layered" || {
  printf 'ci-config: the fixture has no empty testenv_registry to layer over\n' >&2
  exit 1
}

rundir="${RUNNER_TEMP}/spira-run"
sed -i "s|^run = \"/fixture/userhome/spira/run\"\$|run = \"$rundir\"|" "$layered"
grep -q "^run = \"$rundir\"\$" "$layered" || {
  printf 'ci-config: the fixture has no fixture-path run to layer over\n' >&2
  exit 1
}
mkdir -p "$rundir"
