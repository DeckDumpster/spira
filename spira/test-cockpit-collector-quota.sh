#!/usr/bin/env bash
#
# test-cockpit-collector-quota.sh — collector unit CPU limits and probe fault rendering.
#
# 1. UNIT CPUQuota: the collector service must not carry CPUQuota. A quota below the
#    core probe's real cost throttles it until the timeout fires and the snapshot goes
#    stale. Pair: the old 35% value fails this check.
# 2. PASS LOG LINE: collect.sh logs "probe <name> ok <N>s" after a successful run.
#
# The STALE-vs-FAULT badge case moved to test-cockpit-probe-fault.sh's renderer table
# (cluster 8, UC-24, docs/test-plan/cockpit-observability.md) — it is the same "? not 0"-
# shaped question probe-fault already owns, just keyed on a timed-out probe.
#
# defect: sp-onasx
# covers: systemd/spira-cockpit.service spira/collect.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
UNIT="$(dirname "$HERE")/systemd/spira-cockpit.service"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ============================================================
echo "1. CPUQuota: collector service must not carry CPUQuota"
# ============================================================

if [ ! -f "$UNIT" ]; then
    bad "unit file not found" "$UNIT"
else
    # Positive control: a unit with CPUQuota=35% is detected.
    FAKE_UNIT="$TMP/fake-cockpit.service"
    printf '[Service]\nCPUQuota=35%%\nNice=10\n' > "$FAKE_UNIT"
    if grep -qE '^[[:space:]]*CPUQuota=' "$FAKE_UNIT" 2>/dev/null; then
        ok "CPUQuota/positive control: fake unit with CPUQuota=35% is detected"
    else
        bad "CPUQuota/positive control: grep did not find CPUQuota=35% in fake unit" ""
    fi

    # Real check: spira-cockpit.service must have no CPUQuota directive.
    if grep -qE '^[[:space:]]*CPUQuota=' "$UNIT" 2>/dev/null; then
        bad "spira-cockpit.service has CPUQuota — remove it (throttles core probe)" \
            "$(grep 'CPUQuota' "$UNIT")"
    else
        ok "spira-cockpit.service: no CPUQuota"
    fi
fi

# ============================================================
echo
echo "2. Pass log line names probe and elapsed seconds"
# ============================================================

mkdir -p "$TMP/run" "$TMP/bin" "$TMP/home"

MOCK_COCK="$TMP/mock-cockpit.sh"
printf '#!/usr/bin/env bash\ncase "$1" in quick) echo SP_MOCK=1 ;; esac\n' \
    > "$MOCK_COCK" && chmod +x "$MOCK_COCK"

FRAG_DIR="$TMP/frags"
mkdir -p "$FRAG_DIR"
printf '_PROBE_AT=0\n_PROBE_STATUS=never\n_PROBE_KILLED=0\n' > "$FRAG_DIR/testprobe.env"

log_out="$TMP/probe.log"
env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" \
    FRAG_DIR="$FRAG_DIR" COCK="$MOCK_COCK" \
    bash "$HERE/collect.sh" _probe_body_test testprobe 30 quick 2>"$log_out" || true

log_msg="$(cat "$log_out" 2>/dev/null)"
want "pass log: contains probe name"     "testprobe"   "$log_msg"
want "pass log: contains 'ok'"           "ok"          "$log_msg"
want "pass log: contains elapsed suffix" "s"           "$log_msg"
# Format: "collect.sh: probe testprobe ok Ns"
if printf '%s\n' "$log_msg" | grep -qE 'probe testprobe ok [0-9]+s'; then
    ok "pass log: format is 'probe <name> ok <N>s'"
else
    bad "pass log: format wrong" "$log_msg"
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
