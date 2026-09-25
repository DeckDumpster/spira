#!/usr/bin/env bash
# covers: systemd/render.py systemd/concierge.service systemd/beads-push.service
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf 'ok %s\n' "$*"; }
fail() { fail=$((fail+1)); printf 'FAIL %s\n' "$*"; }

# T0 — ExecStart lines in rendered units must not resolve to the git checkout path.
#
# In a split-checkout deployment SPIRA_REPO is the git checkout and SPIRA_PROD is
# the release tree.  Any ExecStart that uses @SPIRA_REPO@ points into the checkout;
# @SPIRA_PROD_ROOT@ (dirname of SPIRA_PROD) is the correct placeholder.

_tmp="$(mktemp -d)"
trap 'rm -rf "$_tmp"' EXIT

_repo="$_tmp/repo"
_prod="$_tmp/releases/current/spira"
_prod_root="$_tmp/releases/current"
mkdir -p "$_repo" "$_prod" "$_prod_root/cockpit"

# Render using render.py directly — the actual module install.sh and unit-ensure.sh
# both call, not a copy of its substitution logic.
render_py() {
    local template="$1"
    python3 "$ROOT/systemd/render.py" "$template" \
        --home "$_repo" --repo "$_repo" --run "$_tmp/run" --db "$_tmp/db" \
        --cockpit "$_prod_root/cockpit" --prod "$_prod" --instance test
}

# Positive control: a synthetic template with @SPIRA_REPO@ must render to the repo path.
# This verifies the detector can fire before we rely on its silence.
_bad="$_tmp/bad.service"
printf '[Service]\nExecStart=@SPIRA_REPO@/something.sh\n' > "$_bad"
_rendered_bad="$(render_py "$_bad")"
if echo "$_rendered_bad" | grep -q "ExecStart=$_repo/"; then
    ok "positive control: @SPIRA_REPO@ renders to checkout path — detector fires"
else
    fail "positive control: @SPIRA_REPO@ did NOT render to checkout path — check is broken"
fi

# T0b/c — service templates must use @SPIRA_PROD_ROOT@, not @SPIRA_REPO@, in ExecStart.
for svc in systemd/concierge.service systemd/beads-push.service; do
    if grep -E '^ExecStart=' "$ROOT/$svc" | grep -q '@SPIRA_REPO@'; then
        fail "$svc: ExecStart uses @SPIRA_REPO@ — resolves to git checkout in release mode"
    else
        ok "$svc: ExecStart does not use @SPIRA_REPO@"
    fi
    if grep -E '^ExecStart=' "$ROOT/$svc" | grep -q '@SPIRA_PROD_ROOT@'; then
        ok "$svc: ExecStart uses @SPIRA_PROD_ROOT@"
    else
        fail "$svc: ExecStart does not use @SPIRA_PROD_ROOT@"
    fi
done

# T0d — rendered concierge.service ExecStart must point into the release root, not the repo.
_rendered_concierge="$(render_py "$ROOT/systemd/concierge.service")"
if echo "$_rendered_concierge" | grep -E '^ExecStart=' | grep -q "$_repo"; then
    fail "concierge.service rendered ExecStart points into git checkout: $_repo"
else
    ok "concierge.service rendered ExecStart does not point into git checkout"
fi
if echo "$_rendered_concierge" | grep -E '^ExecStart=' | grep -q "$_prod_root"; then
    ok "concierge.service rendered ExecStart points into release root"
else
    fail "concierge.service rendered ExecStart does not point into release root: $_prod_root"
fi

# T0e — rendered beads-push.service ExecStart must point into the release root.
_rendered_push="$(render_py "$ROOT/systemd/beads-push.service")"
if echo "$_rendered_push" | grep -E '^ExecStart=' | grep -q "$_repo"; then
    fail "beads-push.service rendered ExecStart points into git checkout: $_repo"
else
    ok "beads-push.service rendered ExecStart does not point into git checkout"
fi
if echo "$_rendered_push" | grep -E '^ExecStart=' | grep -q "$_prod_root"; then
    ok "beads-push.service rendered ExecStart points into release root"
else
    fail "beads-push.service rendered ExecStart does not point into release root: $_prod_root"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
