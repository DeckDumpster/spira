#!/usr/bin/env bash
#
# test-doctor-unit-install.sh — doctor.sh "unit installation" section reports
# enable-set units that are missing from the installation as faults; a complete
# installation reports none.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): a missing unit is
# reported before the clean case is trusted, so silence is meaningful.
#
# THE FIXTURE uses install.sh --render to produce unit files identical to what
# --diff would compare against (law-prefer-the-real-dependency). A stub model
# would reproduce the rendering as the test author remembered it.
#
# GATE: an empty unit directory (fresh host) must not produce FAILs — doctor.sh
# skips the check until install.sh has written at least one unit file.
#
# covers: spira/doctor.sh spira/skew.sh systemd/install.sh systemd/unit-ensure.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-doctor-unit-install.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

BIN="$TMP/bin"
mkdir -p "$BIN" "$TMP/db/.beads" "$TMP/run"
touch "$TMP/watchers-empty"

cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

# Fake systemctl: reports all units as enabled/active so enablement checks pass.
cat > "$BIN/sc" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)  printf 'active\n'; exit 0 ;;
    *"is-enabled"*) printf 'enabled\n'; exit 0 ;;
    *) true ;;
esac
exit 0
MOCK
chmod +x "$BIN/sc"

# Common env for both the reference render and doctor.sh runs — same values so
# the rendered unit content is byte-for-byte identical in both contexts.
# SPIRA_WATCHERS must be the empty file so units.sh includes no watcher units;
# the SPIRA_CONF=/nonexistent path makes conf.sh fall back to defaults derived
# from the real harness location.
COMMON_ENV=(
    HOME="$TMP/home"
    SPIRA_CONF=/nonexistent
    SPIRA_DB="$TMP/db"
    SPIRA_RUN="$TMP/run"
    SPIRA_INSTANCE=prod
    SPIRA_WATCHERS="$TMP/watchers-empty"
    SPIRA_DOLT_DATA=""
    SPIRA_TESTDB_DATA=""
)

DEST="$TMP/home/.config/systemd/user"
mkdir -p "$DEST"

# ==========================================================================
echo
echo "fixture: render all units for comparison:"
# ==========================================================================
rendered="$(env -i PATH="$PATH" "${COMMON_ENV[@]}" \
    bash "$HERE/../systemd/install.sh" --render 2>&1)"
if printf '%s\n' "$rendered" | grep -q "^====="; then
    ok "render produced unit headers"
else
    bad "render produced output" "no ===== headers; install.sh may have failed"
fi

# Write each rendered unit to DEST.
current_unit=""
while IFS= read -r line; do
    if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
        current_unit="${BASH_REMATCH[1]}"
        > "$DEST/$current_unit"
    elif [ -n "$current_unit" ]; then
        printf '%s\n' "$line" >> "$DEST/$current_unit"
    fi
done <<< "$rendered"
installed_count="$(ls "$DEST" | wc -l)"
[ "$installed_count" -gt 0 ] \
    && ok "fixture: wrote $installed_count unit file(s) to DEST" \
    || bad "fixture: wrote rendered units" "no files in $DEST"

# ==========================================================================
echo
echo "positive control — missing unit reported before clean case trusted:"
# ==========================================================================
czar_timer="$DEST/spira-czar-pass-prod.timer"
if [ -f "$czar_timer" ]; then
    cp "$czar_timer" "$TMP/czar-saved"
    rm -f "$czar_timer"

    pos_out="$(env -i PATH="/usr/local/bin:/usr/bin:/bin" "${COMMON_ENV[@]}" \
        SPIRA_PATH="$BIN" SPIRA_SYSTEMCTL="$BIN/sc" \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent SPIRA_NOTIFY=/nonexistent \
        bash "$HERE/doctor.sh" 2>/dev/null || true)"

    want "positive control: FAIL present"           "  FAIL  "                  "$pos_out"
    want "positive control: czar-pass unit named"   "spira-czar-pass-prod.timer" "$pos_out"
    want "positive control: unit-ensure suggested"  "unit-ensure.sh"             "$pos_out"

    # Restore the file.
    cp "$TMP/czar-saved" "$czar_timer"
else
    bad "positive control setup" "spira-czar-pass-prod.timer not found in fixture"
fi

# ==========================================================================
echo
echo "clean case — all units present: no FAIL in unit installation section:"
# ==========================================================================
clean_out="$(env -i PATH="/usr/local/bin:/usr/bin:/bin" "${COMMON_ENV[@]}" \
    SPIRA_PATH="$BIN" SPIRA_SYSTEMCTL="$BIN/sc" \
    SPIRA_BD_PIN="$TMP/run/bd-pin" \
    SPIRA_REPO_MAP=/nonexistent SPIRA_NOTIFY=/nonexistent \
    bash "$HERE/doctor.sh" 2>/dev/null || true)"

ui_out="$(printf '%s\n' "$clean_out" | awk '/^unit installation$/{p=1} p{print} /^[a-z]/{if(p&&!/^unit installation$/)exit}' || true)"
nowant "clean: no FAIL in unit installation section" "  FAIL  " "$ui_out"
want   "clean: OK line present" "  ok  " "$ui_out"

# ==========================================================================
echo
echo "fresh install gate — empty unit dir: check skipped, no FAIL:"
# ==========================================================================
# A host where install.sh has never run has no unit files. The check must skip.
empty_home="$TMP/home-empty"
mkdir -p "$empty_home/.config/systemd/user"
gate_out="$(env -i PATH="/usr/local/bin:/usr/bin:/bin" \
    HOME="$empty_home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_DB="$TMP/db" SPIRA_RUN="$TMP/run" \
    SPIRA_INSTANCE=prod \
    SPIRA_WATCHERS="$TMP/watchers-empty" \
    SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
    SPIRA_PATH="$BIN" SPIRA_SYSTEMCTL="$BIN/sc" \
    SPIRA_BD_PIN="$TMP/run/bd-pin" \
    SPIRA_REPO_MAP=/nonexistent SPIRA_NOTIFY=/nonexistent \
    bash "$HERE/doctor.sh" 2>/dev/null || true)"

gate_ui="$(printf '%s\n' "$gate_out" | awk '/^unit installation$/{p=1} p{print} /^[a-z]/{if(p&&!/^unit installation$/)exit}' || true)"
nowant "fresh install gate: no FAIL" "  FAIL  " "$gate_ui"
want   "fresh install gate: OK or skip line" "  ok  " "$gate_ui"

# ==========================================================================
echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
