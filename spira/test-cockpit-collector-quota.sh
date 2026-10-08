#!/usr/bin/env bash
#
# test-cockpit-collector-quota.sh — collector unit CPU limits and probe fault rendering.
#
# 1. UNIT CPUQuota: the collector service carries a CPUQuota of at least 100%. A quota
#    below the core probe's real cost throttles it until the timeout fires and the snapshot
#    goes stale; none at all lets its bd reads starve the box. Pair: 35% fails this check.
# 2. PASS LOG LINE: cockpit-collect logs "probe <name> ok <N>s" after a successful run.
#
# The STALE-vs-FAULT badge case moved to test-cockpit-probe-fault.sh's renderer table
# (cluster 8, UC-24, docs/test-plan/cockpit-observability.md) — it is the same "? not 0"-
# shaped question probe-fault already owns, just keyed on a timed-out probe.
#
# sp-kt4l3: section 3 ("Probe body's EXIT trap survives function return") retired.
# `_run_probe_body`'s deferred-`local`-in-a-trap defect (sp-2575x) was a bash scoping hazard
# — a trap string re-reading a function-local variable after the function's scope had
# popped. `cockpit-collect`'s Rust port has no traps and no unbound-variable class at all;
# the property is now structural (rung 4 of the ladder), not a thing a test can still find
# broken, so there is nothing left here to assert.
#
# defect: sp-onasx
# tier: T1
# covers: systemd/spira-cockpit.service cockpit-collect/src/supervisor.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
UNIT="$(dirname "$HERE")/systemd/spira-cockpit.service"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ============================================================
echo "1. CPUQuota: collector service is fenced, but not below the core probe's cost"
# ============================================================

quota_pct() { sed -n 's/^[[:space:]]*CPUQuota=\([0-9][0-9]*\)%.*/\1/p' "$1" | head -n1; }

if [ ! -f "$UNIT" ]; then
    bad "unit file not found" "$UNIT"
else
    FAKE_UNIT="$TMP/fake-cockpit.service"
    printf '[Service]\nCPUQuota=35%%\nNice=10\n' > "$FAKE_UNIT"
    fake_q="$(quota_pct "$FAKE_UNIT")"
    if [ "$fake_q" = 35 ] && [ "$fake_q" -lt 100 ]; then
        ok "CPUQuota/positive control: fake unit's 35% is read and judged throttling"
    else
        bad "CPUQuota/positive control: matcher did not read 35% from fake unit" "$fake_q"
    fi

    q="$(quota_pct "$UNIT")"
    if [ -z "$q" ]; then
        bad "spira-cockpit.service has no CPUQuota — the collector's bd reads are unfenced" ""
    elif [ "$q" -lt 100 ]; then
        bad "spira-cockpit.service CPUQuota=$q% is below one core — throttles core probe" "$q"
    else
        ok "spira-cockpit.service: CPUQuota=$q% (fenced, at least one core)"
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
tl_config SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$TMP/no-map"
env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    FRAG_DIR="$FRAG_DIR" COCK="$MOCK_COCK" \
    cockpit-collect _probe_body_test testprobe 30 quick 2>"$log_out" || true

log_msg="$(cat "$log_out" 2>/dev/null)"
want "pass log: contains probe name"     "testprobe"   "$log_msg"
want "pass log: contains 'ok'"           "ok"          "$log_msg"
want "pass log: contains elapsed suffix" "s"           "$log_msg"
# Format: "collect.sh: probe testprobe ok Ns" (cockpit-collect keeps the same wire format).
if printf '%s\n' "$log_msg" | grep -qE 'probe testprobe ok [0-9]+s'; then
    ok "pass log: format is 'probe <name> ok <N>s'"
else
    bad "pass log: format wrong" "$log_msg"
fi


echo
tl_summary
