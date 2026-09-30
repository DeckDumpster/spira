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
. "$HERE/testlib.sh"
UNIT="$(dirname "$HERE")/systemd/spira-cockpit.service"

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
    collect.sh _probe_body_test testprobe 30 quick 2>"$log_out" || true

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

# ============================================================
echo
echo "3. Probe body's EXIT trap survives function return (sp-2575x)"
# ============================================================
# _run_probe_body sets its cleanup trap while "name" is still a local
# variable. A trap deferred with single quotes re-reads that name when the
# (sub)shell actually exits, which is after the function has returned and its
# `local` scope has popped — under set -u that reads as unbound. "line 1" in
# the wild pointed at the trap string's own line count, not the script.

# Positive control: prove this exact shape — a trap set on a `local` inside a
# function, fired after the function returns in its own subshell — really
# does raise "unbound variable" under set -u, so a silent real check below is
# believable.
CONTROL="$TMP/control.sh"
cat > "$CONTROL" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail
f() {
    local name="probe"
    trap 'rm -f "/tmp/sp-2575x-control-$name"' EXIT
}
( f ) &
wait
EOF
chmod +x "$CONTROL"
control_out="$(bash "$CONTROL" 2>&1)"
if printf '%s\n' "$control_out" | grep -qi 'unbound variable'; then
    ok "positive control: deferred local-var EXIT trap reproduces unbound variable"
else
    bad "positive control: deferred local-var EXIT trap did not reproduce the defect" "$control_out"
fi

# Real check: collect.sh's own probe-body trap must not repeat that mistake,
# on both the success path (this section) and the failure path (a probe that
# exits non-zero, which also sets the trap before returning).
run_out="$(env -i PATH="$PATH" HOME="$TMP/home" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" \
    FRAG_DIR="$FRAG_DIR" COCK="$MOCK_COCK" \
    bash "$HERE/collect.sh" _probe_body_test testprobe 30 quick 2>&1)"
if printf '%s\n' "$run_out" | grep -qi 'unbound variable'; then
    bad "collect.sh probe body (success path): EXIT trap raised unbound variable" "$run_out"
else
    ok "collect.sh probe body (success path): EXIT trap did not raise unbound variable"
fi

FAIL_COCK="$TMP/fail-cockpit.sh"
printf '#!/usr/bin/env bash\nexit 1\n' > "$FAIL_COCK" && chmod +x "$FAIL_COCK"
fail_out="$(env -i PATH="$PATH" HOME="$TMP/home" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" \
    FRAG_DIR="$FRAG_DIR" COCK="$FAIL_COCK" \
    bash "$HERE/collect.sh" _probe_body_test testprobe 30 quick 2>&1)"
if printf '%s\n' "$fail_out" | grep -qi 'unbound variable'; then
    bad "collect.sh probe body (failure path): EXIT trap raised unbound variable" "$fail_out"
else
    ok "collect.sh probe body (failure path): EXIT trap did not raise unbound variable"
fi

echo
tl_summary
