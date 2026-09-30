#!/usr/bin/env bash
#
# test-ready-timers.sh — ready.sh's "timers" check: an active spira-*-<instance>.timer
# with no next trigger is exactly the sp-ly6l9 shape (OnBootSec's deadline already past,
# no OnUnitActiveSec activation in this run to measure from) and is-active alone cannot
# see it, since "active (elapsed)" is still an active state.
#
# Split out of test-ready.sh (deleted in round 96 for an unrelated flip in its real-probe
# loom grace case, tracked by sp-o9mr9) so this check keeps a covering suite without the
# path that flaked: every seam below is a deterministic stub, never a real network probe.
#
# tier: T2
# covers: spira/ready.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
BIN="$TMP/bin"; mkdir -p "$BIN"
FAKE_HOME="$TMP/fakehome"
UNIT_DIR="$FAKE_HOME/.config/systemd/user"
mkdir -p "$UNIT_DIR"
FAKE_SPIRA_HOME="$TMP/fakespira"
mkdir -p "$FAKE_SPIRA_HOME"
RUN="$TMP/run"; mkdir -p "$RUN"
DB="$TMP/db"; mkdir -p "$DB/.beads"

# Fake systemctl — is-active/is-enabled controlled by FAKE_SC_ACTIVE/FAKE_SC_ENABLED
# (space-separated unit lists); `show -p <prop> --value <unit>` reports a healthy
# non-zero monotonic elapse for every unit, except one listed in FAKE_SC_ELAPSED, which
# reports both NextElapse properties empty — the sp-ly6l9 defect shape.
cat > "$BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
unit=""
for a in "$@"; do
    case "$a" in
        spira-*|dolt-*) unit="$a" ;;
    esac
done
if [[ "$*" == *"is-active"* ]]; then
    active="${FAKE_SC_ACTIVE:-}"
    if [[ -n "$active" ]] && [[ " $active " == *" $unit "* ]]; then
        [[ "$*" == *"--quiet"* ]] && exit 0
        printf 'active\n'; exit 0
    else
        [[ "$*" == *"--quiet"* ]] && exit 1
        printf 'inactive\n'; exit 1
    fi
fi
if [[ "$*" == *"is-enabled"* ]]; then
    enabled="${FAKE_SC_ENABLED:-}"
    if [[ -n "$enabled" ]] && [[ " $enabled " == *" $unit "* ]]; then
        printf 'enabled\n'; exit 0
    fi
    exit 1
fi
if [[ "$*" == *"show"* ]]; then
    elapsed="${FAKE_SC_ELAPSED:-}"
    if [[ -n "$elapsed" ]] && [[ " $elapsed " == *" $unit "* ]]; then
        printf '0\n'; exit 0
    fi
    printf '123456789\n'; exit 0
fi
exit 0
MOCK
chmod +x "$BIN/systemctl"

# Fake bd — always a readable, empty database.
cat > "$BIN/bd" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--json"*) printf '[]\n'; exit 0 ;;
    *"list"*) exit 0 ;;
    *"ready"*) printf '[]'; exit 0 ;;
    *) exit 0 ;;
esac
MOCK
chmod +x "$BIN/bd"

cat > "$FAKE_SPIRA_HOME/seed.sh" <<'MOCK'
#!/usr/bin/env bash
[ "${1:-}" = "--list" ] && printf 'law-always-present + \n'
exit 0
MOCK
chmod +x "$FAKE_SPIRA_HOME/seed.sh"

# The sentinel is a binary now: ready.sh runs `sentinel --report` by name.
cat > "$BIN/sentinel" <<'MOCK'
#!/usr/bin/env bash
if [ "${1:-}" = "--report" ]; then
    printf '\nOpen beads under sp-test:\n  sp-abc\n'
    exit 0
fi
exit 1
MOCK
chmod +x "$BIN/sentinel"

# Loom probe stub — always answers 200, so ready.sh never reaches its real HTTP probe
# loop (that loop, not this check, is what flaked test-ready.sh in round 96).
cat > "$BIN/loom-probe" <<'MOCK'
#!/usr/bin/env bash
printf '200 42ms\n'
MOCK
chmod +x "$BIN/loom-probe"

cat > "$BIN/tmux" <<'MOCK'
#!/usr/bin/env bash
[[ "$*" == *"list-panes"* ]] && printf 'panel %%1\nhealth %%2\n'
exit 0
MOCK
chmod +x "$BIN/tmux"

cat > "$BIN/fake-agent" <<'MOCK'
#!/usr/bin/env bash
printf 'fake-agent v0\n'
MOCK
chmod +x "$BIN/fake-agent"

printf '[Timer]\nOnBootSec=2min\n' > "$UNIT_DIR/spira-sentinel-prod.timer"
printf 'SP_AT=0\n' > "$RUN/cockpit.env"

# run_ready: run ready.sh with test fixtures; every check but "timers" is fed a healthy
# fixture so the exit-code assertions below reflect only the timers check under test.
run_ready() {
    local extra_env=()
    while [[ "${1:-}" != "--" && $# -gt 0 ]]; do
        extra_env+=("$1"); shift
    done
    [ "${1:-}" = "--" ] && shift
    env -i \
        HOME="$FAKE_HOME" \
        SPIRA_PATH="$BIN" \
        SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$FAKE_SPIRA_HOME" PATH="$BIN:$FAKE_SPIRA_HOME:$PATH" \
        SPIRA_REPO="$TMP" \
        SPIRA_REPO_MAP="$TMP/no-map" \
        SPIRA_RUN="$RUN" \
        SPIRA_DB="$DB" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_BD="$BIN/bd" \
        SPIRA_INSTANCE="prod" \
        SPIRA_LOOM_ADDR="127.0.0.1:8788" \
        SPIRA_LOOM_BUDGET_MS="1500" \
        SPIRA_LOOM_PROBE="$BIN/loom-probe" \
        SPIRA_AGENT="fake-agent" \
        FAKE_SC_ACTIVE="spira-sentinel-prod.timer spira-mail-tidy-prod.timer" \
        FAKE_SC_ENABLED="spira-sentinel-prod.timer" \
        "${extra_env[@]}" \
        ready.sh 2>/dev/null
}

echo "test-ready-timers.sh"
echo

echo "--- timers: elapsed with no next trigger ---"

# POSITIVE CONTROL: a timer is active but reports no next elapse -> FAIL, before
# trusting the healthy case below (law-absence-needs-a-positive-control).
echo "positive control: timer active, elapsed (no next trigger)"
printf '[Timer]\nOnBootSec=3min\nOnUnitActiveSec=15min\n' > "$UNIT_DIR/spira-mail-tidy-prod.timer"
out="$(run_ready "FAKE_SC_ELAPSED=spira-mail-tidy-prod.timer" -- || true)"
want "timers-fail: FAIL line present" \
     "  FAIL  spira-mail-tidy-prod.timer is active but has stopped scheduling" "$out"
want "timers-fail: names the unit"    "spira-mail-tidy-prod.timer"             "$out"

# PASS: same timer, still scheduled (has a next trigger).
echo "timer active, still scheduled"
out="$(run_ready --)"
want "timers-pass: pass line present" \
     "  pass  spira-mail-tidy-prod.timer still scheduled" "$out"
nowant "timers-pass: no FAIL for timer" "  FAIL  spira-mail-tidy-prod.timer"   "$out"

# An inactive timer is not checked — only active ones can be elapsed.
echo "timer file present but not active — not checked"
rm -f "$UNIT_DIR/spira-mail-tidy-prod.timer"
printf '[Timer]\nOnBootSec=3min\nOnUnitActiveSec=15min\n' > "$UNIT_DIR/spira-idle-prod.timer"
out="$(run_ready "FAKE_SC_ELAPSED=spira-idle-prod.timer" --)"
nowant "timers-inactive: no FAIL for inactive timer" "spira-idle-prod.timer" "$out"
rm -f "$UNIT_DIR/spira-idle-prod.timer"

echo ""
echo "--- exit code ---"

echo "exit 1 when a timer has stopped scheduling"
printf '[Timer]\nOnBootSec=3min\nOnUnitActiveSec=15min\n' > "$UNIT_DIR/spira-mail-tidy-prod.timer"
run_ready "FAKE_SC_ELAPSED=spira-mail-tidy-prod.timer" -- >/dev/null 2>&1 \
    && bad "exit-fail: should exit 1 when a timer is elapsed" "exited 0" \
    || ok "exit-fail: exits 1 when a timer is elapsed"

echo "exit 0 when every active timer is still scheduled"
run_ready -- >/dev/null 2>&1 \
    && ok "exit-pass: exits 0 when every timer is scheduled" \
    || bad "exit-pass: should exit 0 when every timer is scheduled" "exited non-zero"

tl_summary
