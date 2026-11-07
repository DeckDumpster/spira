#!/usr/bin/env bash
# tier: T1
# covers: spira/drift.sh spira/owned.sh cockpit-collect/src/* watchtower/src/* sentinel/src/dispatch.rs
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
    env -i SPIRA_RELEASE="$(PATH="$PATH" tl_release)" PATH="$PATH" HOME="$TMP/home" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF=/nonexistent \
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

manifest="$(env -i SPIRA_RELEASE="$(PATH="$PATH" tl_release)" PATH="$PATH" HOME="$TMP/home" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF=/nonexistent bash "$HERE/owned.sh" list 2>/dev/null)"
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
mkdir -p "$UNITDIR/spira-dolt-boot-check-prod.service.d" "$UNITDIR/spira-dolt-boot-check-prod.timer.d"
printf '# installed by install-intake.sh\n' > "$UNITDIR/spira-dolt-boot-check-prod.service.d/50-spira-intake.conf"
printf '# installed by cadence.sh\n'        > "$UNITDIR/spira-dolt-boot-check-prod.timer.d/cadence.conf"
out="$(drift units "$UNITDIR")"; rc=$?
is "sanctioned drop-ins alone: exits 0" "0" "$rc"
nowant "sanctioned drop-ins: not reported UNSHIPPED" "UNSHIPPED" "$out"
rm -rf "$UNITDIR/spira-dolt-boot-check-prod.service.d" "$UNITDIR/spira-dolt-boot-check-prod.timer.d"

# ==========================================================================
echo
echo "units — declared operator-local overrides: accepted, undeclared reported, landed retired:"
# ==========================================================================
LREPO="$TMP/local-repo"
git init -q -b main "$LREPO"
git -C "$LREPO" config user.email t@t; git -C "$LREPO" config user.name t
git -C "$LREPO" remote add origin "$TMP/unused.git"
git -C "$LREPO" commit -q --allow-empty -m base
# "LANDED" IS THE LIFECYCLE RECORD'S STATE (sp-oqf8c): drift.sh asks `spira-lc state <bead>`.
# The stub answers from $LCSTATE/<id> (no file: SUBMITTED, known and not landed).
LCSTATE="$TMP/lcstate"; mkdir -p "$LCSTATE" "$TMP/lcbin"
printf '#!/usr/bin/env bash\n[ "${1:-}" = state ] || exit 2\nif [ -s "%s/${2:-}" ]; then cat "%s/${2:-}"; else echo SUBMITTED; fi\n' \
    "$LCSTATE" "$LCSTATE" > "$TMP/lcbin/spira-lc"
chmod +x "$TMP/lcbin/spira-lc"
PATH="$TMP/lcbin:$PATH"; tl_declare_release

land_base() {
    git -C "$LREPO" update-ref refs/remotes/origin/main HEAD
    git -C "$LREPO" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main
}
land_base

LMANIFEST="$TMP/declared/local-units.toml"
mkdir -p "$TMP/declared" "$UNITDIR/dolt-beads.service.d"
printf '[Unit]\n' > "$UNITDIR/dolt-beads.service.d/refuse-manual-stop.conf"
touch "$UNITDIR/spira-bins-prune.timer"
cat > "$LMANIFEST" <<'TOML'
[[unit]]
path = "dolt-beads.service.d/refuse-manual-stop.conf"
reason = "interim guard"
bead = "sp-local-guard"

[[unit]]
path = "spira-bins-prune.timer"   # trailing comment
reason = "interim prune"
bead = "sp-local-prune"
TOML
ldrift() {
    tl_config SPIRA_LOCAL_UNITS="$LMANIFEST"
    env -i SPIRA_RELEASE="$(PATH="$TMP/lcbin:$PATH" tl_release)" PATH="$TMP/lcbin:$PATH" HOME="$TMP/home" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF=/nonexistent \
        SPIRA_REPO="$LREPO" \
        bash "$HERE/drift.sh" units "$UNITDIR"
}

out="$(ldrift)"; rc=$?
is "declared overrides, bead not landed: exits 0" "0" "$rc"
nowant "declared overrides: not reported" "UNSHIPPED" "$out"
nowant "declared overrides: not retired yet" "RETIRE-NOW" "$out"

touch "$UNITDIR/test-stray.service"
out="$(ldrift)"; rc=$?
is "undeclared file beside declared ones: exits 1" "1" "$rc"
want "undeclared file: named" "test-stray.service" "$out"
want "undeclared file: marked UNSHIPPED" "UNSHIPPED" "$out"
nowant "undeclared file: declared ones still accepted" "refuse-manual-stop" "$out"
rm -f "$UNITDIR/test-stray.service"

touch "$UNITDIR/test-permanent.service"
printf '\n[[unit]]\npath = "test-permanent.service"\nreason = "dead unit, kept"\npermanent = true\n' >> "$LMANIFEST"
out="$(ldrift)"; rc=$?
is "permanent declaration needs no bead: exits 0" "0" "$rc"
nowant "permanent declaration: not reported" "test-permanent" "$out"
rm -f "$UNITDIR/test-permanent.service"

# A commit naming the bead is not landed-ness: only the lifecycle record's LANDED is.
git -C "$LREPO" commit -q --allow-empty -m "spira: land sp-local-guard"
land_base
out="$(ldrift)"; rc=$?
nowant "a naming commit alone does not retire the override" "RETIRE-NOW" "$out"
echo LANDED > "$LCSTATE/sp-local-guard"
out="$(ldrift)"; rc=$?
is "declared override whose bead landed: exits 1" "1" "$rc"
want "landed bead: marked RETIRE-NOW" "RETIRE-NOW" "$out"
want "landed bead: names the file" "refuse-manual-stop.conf" "$out"
want "landed bead: names the bead" "sp-local-guard" "$out"
nowant "other declared override still accepted" "spira-bins-prune.timer" "$out"

printf '[[unit]]\npath = "spira-bins-prune.timer"\nreason = "no bead"\n' > "$LMANIFEST"
out="$(ldrift)"; rc=$?
is "entry without a retiring bead is no declaration: exits 1" "1" "$rc"
want "entry without a bead: reported" "spira-bins-prune.timer" "$out"
want "entry without a bead: says why" "names no bead" "$out"

rm -rf "$UNITDIR/dolt-beads.service.d" "$UNITDIR/spira-bins-prune.timer"

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
echo "cockpit-collect probe drift: the pane's own keys, OK/DIRTY/UNSHIPPED, ? when unreadable:"
# ==========================================================================
drift_keys_out() {
    env -i SPIRA_RELEASE="$(PATH="$PATH" tl_release)" PATH="$PATH" HOME="$TMP/home" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" SPIRA_REPO="$1" \
        cockpit-collect probe drift 2>/dev/null
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
echo "watchtower --drift-check: files one incident per class, deduped by ref:"
# ==========================================================================
MOCK_INC="$TMP/mock-inc.sh"
cat > "$MOCK_INC" <<MOCK
#!/usr/bin/env bash
printf '%s\t%s\n' "\$2" "\${SPIRA_INCIDENT_REF:-}" >> "$TMP/inc-calls"
MOCK
chmod +x "$MOCK_INC"

wt_drift_check() {   # wt_drift_check <repo> <unitdir>
    tl_config SPIRA_RUN="$TMP/run"
    env -i SPIRA_RELEASE="$(PATH="$PATH" tl_release)" PATH="$PATH" HOME="$TMP/home" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" \
        SPIRA_REPO="$1" SPIRA_INCIDENT_SH="$MOCK_INC" \
        watchtower --drift-check 2>&1
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

# A CHECKOUT HALF THAT CANNOT RUN MUST RAISE AN INCIDENT, NOT LOG FOREVER.
wt_drift_blind() {
    tl_config SPIRA_RUN="$TMP/run"
    env -i PATH="$PATH" HOME="$TMP/home" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" \
        SPIRA_INCIDENT_SH="$MOCK_INC" watchtower --drift-check 2>&1
}
rm -rf "$TMP/run/conditions"; rm -f "$TMP/inc-calls"
wt_drift_blind >/dev/null; wt_drift_blind >/dev/null
is "SPIRA_REPO and SPIRA_RELEASE unset: two runs file nothing yet" "" "$(inc_calls)"
# With no declared release there is nothing to file through: the third run names the refusal.
blind_out="$(wt_drift_blind)"
want "SPIRA_REPO and SPIRA_RELEASE unset: the third run refuses to file, naming SPIRA_RELEASE" "SPIRA_RELEASE is not set" "$blind_out"
is "SPIRA_REPO and SPIRA_RELEASE unset: no incident is filed without a release" "" "$(inc_calls)"

rm -rf "$TMP/run/conditions"; rm -f "$TMP/inc-calls"
for _ in 1 2 3 4; do
    rel_out="$(env -i PATH="$PATH" HOME="$TMP/home" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" \
        SPIRA_RELEASE="$(PATH="$PATH" tl_release)" SPIRA_INCIDENT_SH="$MOCK_INC" watchtower --drift-check 2>&1)"
done
is "release directory: checkout half retired, no incident" "" "$(inc_calls)"
[ -z "$(inc_calls)" ] || note "the last run said: $rel_out"

# ==========================================================================
echo
echo "bdi — an uncapped bulk disk is named, a capped one is not:"
# ==========================================================================
SYS="$TMP/bdi"; mkdir -p "$SYS/8:32" "$SYS/8:48"
bdi() {
    env -i SPIRA_RELEASE="$(PATH="$PATH" tl_release)" PATH="$PATH" HOME="$TMP/home" SPIRA_CONF=/nonexistent \
        SPIRA_TOML="$(tl_layer SPIRA_BDI_SYSFS="$SYS" SPIRA_BDI_MAX_RATIO=7 SPIRA_BDI_CAPPED="$CAPPED")" bash "$HERE/drift.sh" "$@"
}
echo 1 > "$SYS/8:32/strict_limit"; echo 7 > "$SYS/8:32/max_ratio"
echo 0 > "$SYS/8:48/strict_limit"; echo 100 > "$SYS/8:48/max_ratio"

CAPPED="8:32"
out="$(bdi bdi)"; rc=$?
is "capped disk: exits 0" "0" "$rc"
want "capped disk: says clean" "clean" "$out"

CAPPED="8:32 8:48"
out="$(bdi bdi)"; rc=$?
is "uncapped disk: exits 1" "1" "$rc"
want "uncapped disk: named with its live values" "UNCAPPED 8:48 (bdi 8:48: strict_limit=0 max_ratio=100, want 1 and 7)" "$out"
nowant "capped sibling: not named" "UNCAPPED 8:32" "$out"

echo 5 > "$SYS/8:32/max_ratio"; CAPPED="8:32"
out="$(bdi bdi)"; rc=$?
is "ratio differing from the configured one: exits 1" "1" "$rc"

CAPPED="8:99"
out="$(bdi bdi 2>&1)"; rc=$?
is "disk with no bdi entry: cannot-check, exit 3" "3" "$rc"

ROOT_SRC="$(findmnt -nro SOURCE -T "$TMP" 2>/dev/null)"
ROOT_ID=""
while read -r row; do
    [ -e "/sys/dev/block/$row/partition" ] || { ROOT_ID="$row"; break; }
done < <(lsblk -snro MAJ:MIN "$ROOT_SRC" 2>/dev/null)
if [ -n "$ROOT_ID" ]; then
    mkdir -p "$SYS/$ROOT_ID"; echo 0 > "$SYS/$ROOT_ID/strict_limit"; echo 100 > "$SYS/$ROOT_ID/max_ratio"
    CAPPED="$TMP"
    out="$(bdi bdi)"; rc=$?
    is "mount point: exits 1" "1" "$rc"
    want "mount point: resolved to the disk's bdi" "UNCAPPED $TMP (bdi $ROOT_ID:" "$out"
    NAME="$(lsblk -dnro NAME "/dev/block/$ROOT_ID")"
    out="$(bdi bdi-rule)"; rc=$?
    is "bdi-rule: exits 0" "0" "$rc"
    is "bdi-rule: names the disk and the configured ratio" \
        "ACTION==\"add|change\", SUBSYSTEM==\"block\", KERNEL==\"$NAME\", ATTR{bdi/strict_limit}=\"1\", ATTR{bdi/max_ratio}=\"7\"" "$out"
else
    skip "mount point and bdi-rule: no block device behind the temp dir here"
fi

CAPPED="8:99"
out="$(bdi bdi-rule 2>&1)"; rc=$?
is "bdi-rule: a disk with no block device exits 3" "3" "$rc"

out="$(bdi nonsense 2>&1)"; rc=$?
want "usage: lists bdi" "drift.sh bdi-rule" "$out"

CAPPED=""
out="$(bdi bdi)"; rc=$?
is "nothing declared: exits 0" "0" "$rc"

tl_summary
