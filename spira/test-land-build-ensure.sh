#!/usr/bin/env bash
#
# test-land-build-ensure.sh — land-build-ensure.sh must rebuild a cargo binary
# that is missing while its systemd unit is enabled, for every binary it names.
#
# sp-2mn5v: spira-czar-pass-prod.service exited 2 for four days because the
# czar-pass binary was never built here — no release installed, and nothing
# ran cargo build in the production checkout. land-build-ensure.sh already
# carries the czar-pass entry that heals this (sp-i84vy), but nothing tested
# that the entry works, or would keep working if a future edit dropped it.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): for each binary,
# first prove build.sh is NOT run when the unit is disabled (or the binary is
# present), then prove it IS run when the binary is missing and the unit is
# enabled. A rebuild trigger that fires unconditionally is not a trigger.
#
# host-reason: SPIRA_REPO is a plain temp dir, not a git checkout, so the
# script's git calls fail closed and take the "binary missing" branch only —
# no real systemd, cargo build, or database required.
#
# covers: spira/land-build-ensure.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL — %s\n' "$1"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

REPO="$TMP/repo"
mkdir -p "$REPO/spira"

BUILD_LOG="$TMP/build.log"
cat > "$REPO/spira/build.sh" <<EOF
#!/usr/bin/env bash
printf 'built\n' >> "$BUILD_LOG"
EOF
chmod +x "$REPO/spira/build.sh"

# A mock systemctl: "is-enabled <unit>" exits 0 only for the unit named in
# ENABLED_UNIT, matching the pattern SPIRA_SYSTEMCTL --user is-enabled expects.
MOCK_SC="$TMP/mock-sc"
cat > "$MOCK_SC" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "--user" ] && [ "$2" = "is-enabled" ]; then
    [ "$3" = "${ENABLED_UNIT:-}" ]
    exit $?
fi
exit 1
EOF
chmod +x "$MOCK_SC"

# A present binary — any executable file — so pairs not under test never
# contribute to _be_missing.
PRESENT_BIN="$TMP/present-bin"
printf '#!/usr/bin/env bash\n' > "$PRESENT_BIN"
chmod +x "$PRESENT_BIN"

ABSENT_BIN="$TMP/no-such-dir/absent-bin"

run_ensure() {
    rm -f "$BUILD_LOG"
    ( cd "$TMP" && env -i \
        PATH="$PATH" \
        SPIRA_REPO="$REPO" \
        SPIRA_SYSTEMCTL="$MOCK_SC" \
        SPIRA_INSTANCE="testinst" \
        ENABLED_UNIT="${ENABLED_UNIT:-}" \
        SPIRA_LOOM_BIN="${SPIRA_LOOM_BIN:-$PRESENT_BIN}" \
        SPIRA_BROKER_BIN="${SPIRA_BROKER_BIN:-$PRESENT_BIN}" \
        SPIRA_PANEL="${SPIRA_PANEL:-$PRESENT_BIN}" \
        SPIRA_CZAR_PASS_BIN="${SPIRA_CZAR_PASS_BIN:-$PRESENT_BIN}" \
        bash "$HERE/land-build-ensure.sh" >"$TMP/out.log" 2>&1 )
}

built() { [ -f "$BUILD_LOG" ]; }

# Four (env var, unit base, unit type) triples, matching land-build-ensure.sh's
# own pairs list exactly — a dropped entry here is exactly the regression this
# suite exists to catch.
TRIPLES="SPIRA_LOOM_BIN:loom:service SPIRA_BROKER_BIN:broker:timer SPIRA_PANEL:cockpit:service SPIRA_CZAR_PASS_BIN:czar-pass:timer"

for triple in $TRIPLES; do
    var="${triple%%:*}"
    rest="${triple#*:}"
    base="${rest%%:*}"
    typ="${rest#*:}"
    unit="spira-${base}-testinst.${typ}"

    # Positive control: binary missing, unit NOT enabled — no rebuild.
    unset ENABLED_UNIT
    eval "export $var=\"\$ABSENT_BIN\""
    run_ensure
    if built; then
        bad "$var: missing + unit disabled must not rebuild"
    else
        ok "$var: missing + unit disabled does not rebuild (positive control)"
    fi
    unset "$var"

    # The fence: binary missing, unit enabled — rebuild fires.
    export ENABLED_UNIT="$unit"
    eval "export $var=\"\$ABSENT_BIN\""
    run_ensure
    if built; then
        ok "$var: missing + unit ($unit) enabled triggers build.sh"
    else
        bad "$var: missing + unit ($unit) enabled must trigger build.sh"
    fi
    unset "$var" ENABLED_UNIT

    # Binary present, unit enabled — no rebuild needed.
    export ENABLED_UNIT="$unit"
    eval "export $var=\"\$PRESENT_BIN\""
    run_ensure
    if built; then
        bad "$var: present binary must not trigger a rebuild"
    else
        ok "$var: present binary does not trigger a rebuild"
    fi
    unset "$var" ENABLED_UNIT
done

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
