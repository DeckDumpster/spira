#!/usr/bin/env bash
# tier: T2
# covers: spira/ci-config.sh spira-config/tests/fixtures/complete.toml
#
# ci-config.sh layers a writable run dir and the testenv registry over the complete fixture.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
CK="$(cd "$HERE/.." && pwd -P)"

grep -q '^run = "/fixture/userhome/spira/run"$' "$CK/spira-config/tests/fixtures/complete.toml" \
    && ok "positive control: the fixture run dir is an unwritable fixture path" \
    || bad "positive control: the fixture run dir is an unwritable fixture path"

mkdir "$W/tmp"
env -i PATH="$PATH" RUNNER_TEMP="$W/tmp" GITHUB_REPOSITORY="Some-Owner/repo" \
    bash "$HERE/ci-config.sh" "$CK"
wantrc "ci-config exits 0" 0 "$?"

out="$W/tmp/ci-config.toml"
want "run is rewritten to a runner dir" "run = \"$W/tmp/spira-run\"" "$(grep '^run = ' "$out")"
nowant "no fixture run path remains" '"/fixture/userhome/spira/run"' "$(grep '^run = ' "$out")"
[ -d "$W/tmp/spira-run" ] && ok "the run dir exists" || bad "the run dir exists"
want "registry is still layered" 'testenv_registry = "ghcr.io/some-owner"' "$(cat "$out")"


tl_summary
