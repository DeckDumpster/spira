#!/usr/bin/env bash
#
# test-doctor-hotfix.sh — doctor.sh's hotfix check reads `release status` verbatim: OK
#   with no hotfix, WARN once one stands, FAIL once its ALERT line joins it.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the no-hotfix OK case is
# verified only after the WARN and FAIL cases prove the check actually fires, so an OK that
# fires unconditionally cannot pass by accident.
#
# `release` itself is mocked here: this suite is doctor.sh's own contract with `release
# status` text (RUNNING UNLANDED / ALERT), not release's age-and-threshold arithmetic —
# that lives in release's own `cargo test -p release` (sp-6p20x, DESIGN.md "Hotfix").
#
# No database required; the check only shells out to `release status`.
#
# tier: T1
# covers: spira/doctor.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
TOOLS="$(dirname "$(command -v spira-config)"):$HERE"

echo "test-doctor-hotfix.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

mkdir -p "$TMP/bin" "$TMP/db/.beads" "$TMP/run" "$TMP/home"

cat > "$TMP/bin/bd" <<'FAKEBD'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--json"*)  printf '[{"id":"sp-test"}]\n'; exit 0 ;;
    *"show"*"sp-test"*) printf '[{"id":"sp-test"}]\n'; exit 0 ;;
    *)                  exit 0 ;;
esac
FAKEBD
chmod +x "$TMP/bin/bd"

cat > "$TMP/bin/fake-notify" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$TMP/bin/fake-notify"

cat > "$TMP/bin/systemctl" <<'SH'
#!/usr/bin/env bash
printf 'enabled\n'; exit 0
SH
chmod +x "$TMP/bin/systemctl"

# The mock `release` this suite drives: $RELEASE_STATUS_OUT is the exact stdout doctor.sh's
# doctor_check_hotfix must treat as authoritative.
cat > "$TMP/bin/release" <<'SH'
#!/usr/bin/env bash
[ "${1:-}" = status ] || exit 2
printf '%s\n' "${RELEASE_STATUS_OUT:-current none}"
SH
chmod +x "$TMP/bin/release"

run_doctor() {
    env -i \
        PATH="$TMP/bin:$TOOLS:/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$TMP/bin" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_BD="$TMP/bin/bd" \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY="$TMP/bin/fake-notify" \
        SPIRA_SYSTEMCTL="$TMP/bin/systemctl" \
        SPIRA_GOAL=sp-test \
        SPIRA_COCKPIT="$TMP/run" \
        SPIRA_SNAP_STALE_S=60 \
        RELEASE_STATUS_OUT="$1" \
        doctor.sh 2>/dev/null || true
}

SHA="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

# ==========================================================================
echo
echo "positive control — a standing hotfix past threshold FAILS, naming ALERT (seen first):"
# ==========================================================================
fail_out="$(run_doctor "$(printf 'current abc\nRUNNING UNLANDED %s: fix it (since 2026-09-30T00:00:00Z)\nALERT hotfix %s standing 5h >= threshold 4h\n' "$SHA" "$SHA")")"
want   "past threshold: hotfix section runs"     "hotfix"                       "$fail_out"
want   "past threshold: FAIL fires"               "FAIL  RUNNING UNLANDED $SHA" "$fail_out"
want   "past threshold: names the ALERT line"     "ALERT hotfix $SHA"            "$fail_out"
nowant "past threshold: no ok line"               "ok    no hotfix standing"     "$fail_out"

# ==========================================================================
echo
echo "a standing hotfix under threshold WARNs, not FAILs:"
# ==========================================================================
warn_out="$(run_doctor "$(printf 'current abc\nRUNNING UNLANDED %s: fix it (since 2026-09-30T00:00:00Z)\n' "$SHA")")"
want   "under threshold: WARN fires"       "warn  RUNNING UNLANDED $SHA" "$warn_out"
nowant "under threshold: no FAIL"          "FAIL  RUNNING UNLANDED"      "$warn_out"
nowant "under threshold: no ok line"       "ok    no hotfix standing"    "$warn_out"

# ==========================================================================
echo
echo "no hotfix standing renders ok, not silence:"
# ==========================================================================
ok_out="$(run_doctor "$(printf 'current abc\n')")"
want   "no hotfix: ok line present"  "ok    no hotfix standing" "$ok_out"
nowant "no hotfix: no WARN"          "RUNNING UNLANDED"          "$ok_out"
nowant "no hotfix: no FAIL"          "FAIL  RUNNING UNLANDED"    "$ok_out"

echo
tl_summary
