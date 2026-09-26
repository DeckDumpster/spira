#!/usr/bin/env bash
#
# test-deploy-preflight-new-unit.sh — a release that adds a systemd unit must not refuse
# to deploy itself (github:DeckDumpster/spira#314 / sp-fco5h).
#
#   ./test-deploy-preflight-new-unit.sh
#
# WHAT THIS GUARDS
# ----------------
# deploy.sh step 7 runs `SPIRA_DOCTOR=1 doctor.sh | grep '^  FAIL  '` before activation and
# refuses the deploy on any match. doctor.sh used to have a unit-installation section that
# diffed the box's installed units against the templates on disk (install.sh --diff, exposed
# standalone as `skew.sh units`); a unit the incoming release adds is, by definition, not yet
# installed, so that section reported it MISSING — a FAIL — before the step that would have
# installed it (step 11). Any release that added a unit refused to deploy itself.
#
# doctor.sh no longer has that section (sp-utt1i: doctor becomes runtime-health-only; unit
# installation is a host-preflight question now, not a running-box one). This suite locks
# that in: it proves the "unit not installed" condition is real and detectable by the
# mechanism the bug report named, then proves doctor.sh — what step 7 actually runs — does
# not FAIL on it.
#
# THE POSITIVE CONTROL COMES FIRST (law-absence-needs-a-positive-control). Before trusting
# doctor.sh's silence, `install.sh --diff` is shown to report MISSING for the very unit this
# suite then withholds from doctor.sh's environment — proving the box state really is the
# "release adds a unit the box lacks" case, not a fixture that never had a chance to fail.
#
# THE FIXTURE IS BUILT FROM THE REAL INSTALLER (law-prefer-the-real-dependency), the same
# approach as test-unit-drift.sh: real templates, real install.sh, a DEST this test controls.
#
# tier: T1
# covers: spira/deploy.sh spira/doctor.sh systemd/install.sh spira/skew.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-deploy-preflight-new-unit.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Fixture: a real harness tree install.sh can render templates from, per test-unit-drift.sh.
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
mkdir -p "$FIXTURE/systemd" "$FIXTURE/spira"
for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer; do
    [ -e "$f" ] || continue
    ln -s "$f" "$FIXTURE/systemd/$(basename "$f")"
done
ln -s "$HERE/../systemd/install.sh" "$FIXTURE/systemd/install.sh"
for f in conf.sh watchd.sh lib.sh suite-covers.sh; do
    [ -e "$HERE/$f" ] && ln -s "$HERE/$f" "$FIXTURE/spira/$f"
done
printf '# empty — test fixture\n' > "$FIXTURE/spira/watchers"
printf '# empty\n' > "$FIXTURE/spira/repo-map.example"

DEST="$TMP/home/.config/systemd/user"
mkdir -p "$DEST" "$TMP/home"

# conf.sh rebuilds PATH from SPIRA_PATH + $HOME/.local/bin + the system dirs (never inherits
# the caller's PATH), so a fresh, empty $HOME loses whatever directory this session's dolt
# lives in unless it is handed through explicitly.
DOLT_DIR="$(dirname "$(command -v dolt 2>/dev/null || echo /nonexistent/dolt)")"

inst() {
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$DOLT_DIR" \
        SPIRA_WATCHERS="$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        bash "$FIXTURE/systemd/install.sh" "$@" 2>&1
}

# Render every template and install it, simulating a box fully caught up on the
# currently-active release — nothing here is new or missing yet.
rendered="$(inst --render)"
current_unit=""
while IFS= read -r line; do
    if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
        current_unit="${BASH_REMATCH[1]}"
        > "$DEST/$current_unit"
    elif [ -n "$current_unit" ]; then
        printf '%s\n' "$line" >> "$DEST/$current_unit"
    fi
done <<< "$rendered"
[ "$(find "$DEST" -maxdepth 1 -type f | wc -l)" -gt 0 ] \
    && ok "fixture: rendered units installed into DEST" \
    || bad "fixture: rendered units installed into DEST" "no files in $DEST"

# ==========================================================================
echo
echo "positive control — fully-installed box: install.sh --diff reports clean:"
# ==========================================================================
diff_out="$(inst --diff)"; rc=$?
is   "fixture sane: diff exits 0 before any unit is withheld" "0" "$rc"
want "fixture sane: diff says units match" "installed units match" "$diff_out"

# ==========================================================================
echo
echo "the release adds a unit — withhold one installed unit, simulating a box that has"
echo "never had it (exactly what a release adding a new unit looks like):"
# ==========================================================================
new_unit_path="$(find "$DEST" -maxdepth 1 -name '*.service' -type f | head -1)"
new_unit_name="$(basename "${new_unit_path:-}")"
[ -n "$new_unit_name" ] || bad "fixture: found a unit to withhold" "no .service file in $DEST"
rm -f "$new_unit_path"

diff_out="$(inst --diff)"; rc=$?
is   "install.sh --diff: MISSING is detected — not a silent pass" "1" "$rc"
want "install.sh --diff: names the withheld unit as MISSING" "MISSING" "$diff_out"
want "install.sh --diff: names the withheld unit itself" "$new_unit_name" "$diff_out"

skew_out="$(env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_PATH="$DOLT_DIR" \
    SPIRA_HOME="$FIXTURE/spira" SPIRA_REPO="$FIXTURE" \
    SPIRA_WATCHERS="$FIXTURE/spira/watchers" \
    SPIRA_DOLT_DATA="" \
    SPIRA_TESTDB_DATA="" \
    bash "$HERE/skew.sh" units)"; skew_rc=$?
is   "skew.sh units: also catches the withheld unit" "1" "$skew_rc"
want "skew.sh units: also names it MISSING" "MISSING" "$skew_out"

# ==========================================================================
echo
echo "doctor.sh — what deploy.sh step 7 actually runs — must not FAIL on it:"
# ==========================================================================
# A clean, minimal environment (per test-doctor-events-probe.sh's server-mode fixture) so any
# FAIL that appears is attributable to the withheld unit, not to unrelated missing plumbing.
DB="$TMP/db"; mkdir -p "$DB/.beads"
cat > "$TMP/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf 'Schema already at v61\n'; exit 0 ;;
    *list*--json*|*--json*list*) printf '[{"id":"sp-real1"}]\n'; exit 0 ;;
esac
_sql=""; _next=0
for a in "$@"; do [ "$_next" = 1 ] && { _sql="$a"; break; }; [ "$a" = "sql" ] && _next=1; done
[ -n "$_sql" ] || exit 0
case "$_sql" in
    INSERT*)   exit 0 ;;
    SELECT\ COUNT*) printf 'COUNT(*)\n--------\n1\n(1 rows)\n'; exit 0 ;;
esac
exit 0
FAKESCRIPT
chmod +x "$TMP/bd"

cat > "$TMP/systemctl" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"list-units"*"--state=failed"*) exit 0 ;;
esac
exit 0
FAKESCRIPT
chmod +x "$TMP/systemctl"

mkdir -p "$TMP/run" "$TMP/doctor-home"
touch "$TMP/run/cockpit.env"

run_doctor() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/doctor-home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$TMP" \
        SPIRA_SYSTEMCTL="$TMP/systemctl" \
        SPIRA_BD="$TMP/bd" \
        SPIRA_DB="$DB" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_OPERATED=0 \
        SPIRA_DOCTOR=1 \
        SPIRA_DOLT_DATA="" \
        bash "$HERE/doctor.sh" 2>&1
}

doctor_out="$(run_doctor)"; doctor_rc=$?
pre_deploy_fails="$(printf '%s\n' "$doctor_out" | grep '^  FAIL  ')" || true

# This is deploy.sh step 7, verbatim: refuse only if grep for '^  FAIL  ' finds anything.
[ -z "$pre_deploy_fails" ] \
    && ok  "deploy.sh step 7: pre-deploy doctor has no FAIL — the deploy proceeds" \
    || bad "deploy.sh step 7: pre-deploy doctor has no FAIL — the deploy proceeds" \
           "$pre_deploy_fails"
nowant "doctor.sh: does not name the withheld unit at all" "$new_unit_name" "$doctor_out"
nowant "doctor.sh: no MISSING finding" "MISSING" "$doctor_out"
is     "doctor.sh: exits 0 (only remaining fault would be unrelated)" "0" "$doctor_rc"

echo
tl_summary
