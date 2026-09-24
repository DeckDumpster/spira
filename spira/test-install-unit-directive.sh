#!/usr/bin/env bash
# covers: systemd/install.sh systemd/concierge.service systemd/beads-push.service
set -uo pipefail

cd "$(git rev-parse --show-toplevel)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf 'ok %s\n' "$*"; }
fail() { fail=$((fail+1)); printf 'FAIL %s\n' "$*"; }

# Minimal stubs so install.sh --render does not need a real installation
_tmp="$(mktemp -d)"
trap 'rm -rf "$_tmp"' EXIT

_repo="$_tmp/repo"
_prod="$_tmp/releases/current/spira"
mkdir -p "$_repo" "$_prod" "$_tmp/releases/current/cockpit"

# install.sh --render needs SPIRA_REPO to be a path, but not a git checkout;
# it passes the value to Python argv, which substitutes @SPIRA_REPO@ literally.
# We construct a split-checkout scenario: SPIRA_REPO = git checkout, SPIRA_PROD
# = a separate release directory (not under _repo).

render() {
    local template="$1"
    bash systemd/install.sh --render \
        SPIRA_HOME="$_repo" \
        SPIRA_REPO="$_repo" \
        SPIRA_RUN="$_tmp/run" \
        SPIRA_DB="$_tmp/db" \
        SPIRA_COCKPIT="$_tmp/releases/current/cockpit" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        DOLT="" \
        SPIRA_PROD="$_prod" \
        SPIRA_INSTANCE="test" \
        SPIRA_TESTDB_PORT="" \
        SPIRA_SUPERVISE_BIN="" \
        SPIRA_SNAP_STALE_S="" \
        "$template" 2>/dev/null
}

# T0a — positive control: verify the detector fires when ExecStart contains _repo.
# Construct a synthetic unit that uses SPIRA_REPO directly so we know the check works.
_bad_unit="$_tmp/bad-test.service"
printf '[Service]\nExecStart=@SPIRA_REPO@/something.sh\n' > "$_bad_unit"
_rendered_bad="$(render "$_bad_unit")"
if echo "$_rendered_bad" | grep -q "ExecStart=$_repo"; then
    ok "positive control: ExecStart with checkout path is detectable"
else
    fail "positive control: ExecStart with checkout path was NOT detected — check broken"
fi

# T0b — concierge.service ExecStart must not point into the git checkout
_rendered_concierge="$(render systemd/concierge.service)"
if echo "$_rendered_concierge" | grep -E '^ExecStart=' | grep -qF "$_repo/"; then
    fail "concierge.service ExecStart points into git checkout: $_repo"
else
    ok "concierge.service ExecStart does not point into git checkout"
fi

# T0c — beads-push.service ExecStart must not point into the git checkout
_rendered_push="$(render systemd/beads-push.service)"
if echo "$_rendered_push" | grep -E '^ExecStart=' | grep -qF "$_repo/"; then
    fail "beads-push.service ExecStart points into git checkout: $_repo"
else
    ok "beads-push.service ExecStart does not point into git checkout"
fi

# T0d — concierge.service ExecStart must point into the release root
_release_root="$(dirname "$_prod")"
if echo "$_rendered_concierge" | grep -E '^ExecStart=' | grep -qF "$_release_root/"; then
    ok "concierge.service ExecStart points into release root"
else
    fail "concierge.service ExecStart does not point into release root: $_release_root"
fi

# T0e — beads-push.service ExecStart must point into the release root
if echo "$_rendered_push" | grep -E '^ExecStart=' | grep -qF "$_release_root/"; then
    ok "beads-push.service ExecStart points into release root"
else
    fail "beads-push.service ExecStart does not point into release root: $_release_root"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
