#!/usr/bin/env bash
# tier: T1
# covers: spira/drift.sh spira/owned.sh spira/cockpit.sh spira/watchtower.sh spira/collect.sh
#         spira/sentinel.sh
#
# test-drift.sh — drift.sh finds an untracked file in a production checkout and an unshipped
# drop-in in the installed unit directory; a clean checkout and unit dir report nothing.
#
#   ./test-drift.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# sp-81ph9: an untracked stray suite sat in the production checkout and made a suite lister
# that reads from disk fail hourly, while the batch tree (built from git) never saw it — four
# beads misdiagnosed the failure before anyone asked git. sp-39gqq: an untracked systemd
# drop-in changed a unit's stop behaviour with no trace in the repo. Neither was visible to
# anything that reasons from git or from the repo's own unit manifest. This suite proves
# drift.sh would have caught both.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION IN EACH SECTION (law-absence-needs-a-positive-
# control): a clean checkout/unit dir must report nothing BEFORE a planted offender is
# trusted to be caught, or "found nothing" and "cannot find anything" are indistinguishable.
#
# THE UNIT MANIFEST IS THE REAL ONE. checkout() runs real git in a throwaway repo; units()
# calls the real owned.sh against this tree's real systemd/units.sh, and the fixture seeds
# every unit basename owned.sh itself reports expected — not a hand-modelled list that would
# drift from the installer it is meant to check (law-prefer-the-real-dependency).
#
# THE ENVIRONMENT IS EXPLICIT AND MINIMAL. HOME points at a temp directory so the unit dir
# resolves there, never at the operator's own. SPIRA_CONF is nonexistent so no box
# configuration leaks in (law-gates-run-in-a-clean-environment).
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-drift.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

drift() {
    env -i PATH="$PATH" HOME="$TMP/home" SPIRA_CONF=/nonexistent \
        bash "$HERE/drift.sh" "$@"
}

# ==========================================================================
echo
echo "checkout — a clean tree reports nothing, an untracked file is caught:"
# ==========================================================================
REPO="$TMP/repo"
git init -q "$REPO"
git -C "$REPO" config user.email t@example.com
git -C "$REPO" config user.name t
echo hello > "$REPO/tracked.txt"
mkdir -p "$REPO/spira"
echo hello > "$REPO/spira/existing.sh"
git -C "$REPO" add tracked.txt spira/existing.sh
git -C "$REPO" commit -qm init >/dev/null

out="$(drift checkout "$REPO")"; rc=$?
is "clean checkout: exits 0" "0" "$rc"
want "clean checkout: says clean" "clean" "$out"

echo x > "$REPO/spira/test-stray.sh"
out="$(drift checkout "$REPO")"; rc=$?
is "untracked stray file: exits 1" "1" "$rc"
want "untracked stray file: named" "spira/test-stray.sh" "$out"
want "untracked stray file: marked DIRTY" "DIRTY" "$out"
rm -f "$REPO/spira/test-stray.sh"

echo appended >> "$REPO/tracked.txt"
out="$(drift checkout "$REPO")"; rc=$?
is "modified tracked file: exits 1" "1" "$rc"
want "modified tracked file: named" "tracked.txt" "$out"
git -C "$REPO" checkout -q -- tracked.txt

out="$(drift checkout "$REPO")"; rc=$?
is "back to clean: exits 0" "0" "$rc"

# ==========================================================================
echo
echo "checkout — not a git repository at all is 'could not check', not clean:"
# ==========================================================================
mkdir -p "$TMP/notrepo"
out="$(drift checkout "$TMP/notrepo")"; rc=$?
is "not a repo: exits 3" "3" "$rc"

# ==========================================================================
echo
echo "units — a matching unit dir reports nothing, an unshipped drop-in is caught:"
# ==========================================================================
UNITDIR="$TMP/home/.config/systemd/user"
mkdir -p "$UNITDIR"

manifest="$(env -i PATH="$PATH" HOME="$TMP/home" SPIRA_CONF=/nonexistent bash "$HERE/owned.sh" list 2>/dev/null)"
[ -n "$manifest" ] || bail "owned.sh list produced nothing — cannot build the fixture"
while IFS='|' read -r kind id loc phase retention; do
    [ "$kind" = unit ] || continue
    touch "$UNITDIR/$(basename "$loc")"
done <<< "$manifest"

out="$(drift units "$UNITDIR")"; rc=$?
is "clean unit dir: exits 0" "0" "$rc"
want "clean unit dir: says clean" "clean" "$out"

mkdir -p "$UNITDIR/dolt-beads.service.d"
printf '[Unit]\nRefuseManualStop=yes\n' > "$UNITDIR/dolt-beads.service.d/refuse-manual-stop.conf"
out="$(drift units "$UNITDIR")"; rc=$?
is "unshipped drop-in: exits 1" "1" "$rc"
want "unshipped drop-in: named" "dolt-beads.service.d/refuse-manual-stop.conf" "$out"
want "unshipped drop-in: marked UNSHIPPED" "UNSHIPPED" "$out"
rm -rf "$UNITDIR/dolt-beads.service.d"

touch "$UNITDIR/test-stray.service"
out="$(drift units "$UNITDIR")"; rc=$?
is "unshipped unit file: exits 1" "1" "$rc"
want "unshipped unit file: named" "test-stray.service" "$out"
rm -f "$UNITDIR/test-stray.service"

out="$(drift units "$UNITDIR")"; rc=$?
is "back to clean: exits 0" "0" "$rc"

# ==========================================================================
echo
echo "units — sanctioned drop-ins (intake, cadence) are not flagged:"
# ==========================================================================
mkdir -p "$UNITDIR/spira-groom-prod.service.d" "$UNITDIR/spira-groom-prod.timer.d"
printf '# installed by install-intake.sh\n' > "$UNITDIR/spira-groom-prod.service.d/50-spira-intake.conf"
printf '# installed by cadence.sh\n'        > "$UNITDIR/spira-groom-prod.timer.d/cadence.conf"
out="$(drift units "$UNITDIR")"; rc=$?
is "sanctioned drop-ins alone: exits 0" "0" "$rc"
nowant "sanctioned drop-ins: not reported UNSHIPPED" "UNSHIPPED" "$out"
rm -rf "$UNITDIR/spira-groom-prod.service.d" "$UNITDIR/spira-groom-prod.timer.d"

# ==========================================================================
echo
echo "units — a missing unit directory is 'could not check', not clean:"
# ==========================================================================
out="$(drift units "$TMP/no-such-unitdir")"; rc=$?
is "missing unit dir: exits 3" "3" "$rc"

# ==========================================================================
echo
echo "check — combines both; clean is 0, either drift is 1, either failure is 3:"
# ==========================================================================
out="$(drift check "$REPO" "$UNITDIR")"; rc=$?
is "check: both clean exits 0" "0" "$rc"

echo x > "$REPO/spira/test-stray.sh"
out="$(drift check "$REPO" "$UNITDIR")"; rc=$?
is "check: checkout drift alone exits 1" "1" "$rc"
want "check: names the checkout finding" "DIRTY" "$out"
rm -f "$REPO/spira/test-stray.sh"

touch "$UNITDIR/test-stray.service"
out="$(drift check "$REPO" "$UNITDIR")"; rc=$?
is "check: unit drift alone exits 1" "1" "$rc"
want "check: names the unit finding" "UNSHIPPED" "$out"
rm -f "$UNITDIR/test-stray.service"

out="$(drift check "$TMP/notrepo" "$UNITDIR")"; rc=$?
is "check: an unreadable half exits 3" "3" "$rc"

# ==========================================================================
echo
echo "cockpit.sh drift: the pane's own keys, OK/DIRTY/UNSHIPPED, ? when unreadable:"
# ==========================================================================
drift_keys_out() {
    env -i PATH="$PATH" HOME="$TMP/home" SPIRA_CONF=/nonexistent SPIRA_REPO="$1" \
        bash "$HERE/cockpit.sh" drift 2>/dev/null
}

out="$(drift_keys_out "$REPO")"
want "clean: SP_CHECKOUT_DRIFT=OK"  "SP_CHECKOUT_DRIFT=OK"  "$out"
want "clean: SP_UNIT_DRIFT=OK"      "SP_UNIT_DRIFT=OK"      "$out"

echo x > "$REPO/spira/test-stray.sh"
out="$(drift_keys_out "$REPO")"
want "dirty checkout: SP_CHECKOUT_DRIFT names the count" "SP_CHECKOUT_DRIFT=DIRTY:1" "$out"
rm -f "$REPO/spira/test-stray.sh"

out="$(drift_keys_out "$TMP/notrepo")"
want "unreadable checkout: SP_CHECKOUT_DRIFT=?" "SP_CHECKOUT_DRIFT=?" "$out"

# ==========================================================================
echo
echo "watchtower.sh --drift-check: files one incident per class, deduped by ref:"
# ==========================================================================
MOCK_INC="$TMP/mock-inc.sh"
cat > "$MOCK_INC" <<MOCK
#!/usr/bin/env bash
printf '%s\t%s\n' "\$2" "\${SPIRA_INCIDENT_REF:-}" >> "$TMP/inc-calls"
MOCK
chmod +x "$MOCK_INC"

wt_drift_check() {   # wt_drift_check <repo> <unitdir>
    env -i PATH="$PATH" HOME="$TMP/home" SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_REPO="$1" SPIRA_INCIDENT_SH="$MOCK_INC" \
        bash "$HERE/watchtower.sh" --drift-check 2>&1
}
inc_calls() { cat "$TMP/inc-calls" 2>/dev/null || true; }

mkdir -p "$TMP/run"
rm -f "$TMP/inc-calls"
wt_drift_check "$REPO" "$UNITDIR" >/dev/null
is "both clean: no incident filed" "" "$(inc_calls)"

echo x > "$REPO/spira/test-stray.sh"
rm -f "$TMP/inc-calls"
wt_drift_check "$REPO" "$UNITDIR" >/dev/null
want "checkout drift: files under the checkout-drift ref" "incident:checkout-drift" "$(inc_calls)"
is "checkout drift: exactly one incident" "1" "$(wc -l < "$TMP/inc-calls" 2>/dev/null || echo 0)"
rm -f "$REPO/spira/test-stray.sh"

touch "$UNITDIR/test-stray.service"
rm -f "$TMP/inc-calls"
wt_drift_check "$REPO" "$UNITDIR" >/dev/null
want "unit drift: files under the unit-drift ref" "incident:unit-drift" "$(inc_calls)"
is "unit drift: exactly one incident" "1" "$(wc -l < "$TMP/inc-calls" 2>/dev/null || echo 0)"
rm -f "$UNITDIR/test-stray.service"

# A HALTED WORLD MUST NOT FILE — same guard every other watchtower detector honours.
echo x > "$REPO/spira/test-stray.sh"
touch "$TMP/run/world.halted"
rm -f "$TMP/inc-calls"
wt_drift_check "$REPO" "$UNITDIR" >/dev/null
is "halted world: no incident filed" "" "$(inc_calls)"
rm -f "$TMP/run/world.halted" "$REPO/spira/test-stray.sh"

tl_summary
