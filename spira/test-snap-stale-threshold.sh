#!/usr/bin/env bash
#
# test-snap-stale-threshold.sh — one key (SPIRA_SNAP_STALE_S) drives all three readers.
#
# Proves: when SPIRA_SNAP_STALE_S=7, a 20s-old snapshot is a fault in health.sh,
# watchtower.sh, and doctor.sh; a freshly written snapshot is not.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): stale case runs
# before fresh so a reader that never fires cannot silently pass the clean assertions.
#
# covers: cockpit/health.sh spira/watchtower.sh spira/doctor.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PANE="$HERE/../cockpit/health.sh"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

echo "test-snap-stale-threshold.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
NOW="$(date +%s)"
STALE_AT=$(( NOW - 20 ))

mkdir -p "$TMP/run" "$TMP/bin" "$TMP/db/.beads" "$TMP/home"

# ------ shared mock programs -----------------------------------------------
# Minimal fake bd for doctor.sh path
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

# Minimal suites stub so watchtower --show completes quickly
cat > "$TMP/bin/suites.sh" <<'SH'
#!/usr/bin/env bash
printf '  suites in the tree                  0   (0 gated, 0 timed)\n'
SH
chmod +x "$TMP/bin/suites.sh"

# ------ reader helpers -------------------------------------------------------
run_health() {
    env -i PATH="$PATH" HOME="$TMP/home" TERM=dumb LC_ALL=C.UTF-8 \
        SPIRA_CONF=/nonexistent SPIRA_REPO="$TMP" SPIRA_RUN="$TMP/run" \
        SPIRA_SYSTEMCTL="$TMP/bin/systemctl" \
        SPIRA_SNAP_STALE_S=7 \
        bash "$PANE" once 40 0 2>/dev/null \
      | sed 's/\x1b\[[?0-9;]*[a-zA-Z]//g'
}

run_watchtower() {  # run_watchtower [extra env...]
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_WATCH_GATE_WINDOW=3600 \
        SPIRA_SUITES_SH="$TMP/bin/suites.sh" \
        SPIRA_SNAP_STALE_S=7 \
        "$@" bash "$HERE/watchtower.sh" --show 2>/dev/null
}

run_doctor() {
    env -i \
        PATH="$TMP/bin:/usr/local/bin:/usr/bin:/bin" \
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
        SPIRA_SNAP_STALE_S=7 \
        "$@" bash "$HERE/doctor.sh" 2>/dev/null || true
}

# ===========================================================================
echo
echo "positive control — stale snapshot (20s old, threshold 7s) fires fault in all three:"
# ===========================================================================

printf 'SP_AT=%s\n' "$STALE_AT" > "$TMP/run/cockpit.env"
touch -d "20 seconds ago" "$TMP/run/cockpit.env"

health_stale="$(run_health)"
want "health.sh: stale fires FAULT marker"  "FAULT ("  "$health_stale"

wt_stale="$(run_watchtower)"
want "watchtower.sh: stale fires FAULT"  "FAULT ("  "$wt_stale"

doc_stale="$(run_doctor)"
want "doctor.sh: stale fires FAIL"  "cockpit snapshot stale"  "$doc_stale"

# ===========================================================================
echo
echo "fresh snapshot (written now, threshold 7s) — none of the three fires:"
# ===========================================================================

printf 'SP_AT=%s\n' "$NOW" > "$TMP/run/cockpit.env"

health_fresh="$(run_health)"
nowant "health.sh: fresh — no FAULT marker"  "FAULT ("  "$health_fresh"

wt_fresh="$(run_watchtower)"
nowant "watchtower.sh: fresh — no FAULT"  "FAULT ("  "$wt_fresh"

doc_fresh="$(run_doctor)"
nowant "doctor.sh: fresh — no stale FAIL"  "cockpit snapshot stale"  "$doc_fresh"
want   "doctor.sh: fresh — OK line present"  "cockpit snapshot fresh"  "$doc_fresh"

echo
printf 'test-snap-stale-threshold: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
