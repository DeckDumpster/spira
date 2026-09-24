#!/usr/bin/env bash
#
# test-watchtower-disk-memory.sh — disk and memory are watchtower vital signs.
#
#   ./test-watchtower-disk-memory.sh
#
# WHAT THIS SUITE IS FOR
# -----------------------
# watchtower.sh now computes disk usage (on / and on the workspaces volume) and memory
# availability itself, through SPIRA_DF and SPIRA_MEMINFO seams — the same idiom SPIRA_GH
# and SPIRA_BD carry for their own commands. A reading at or above its warn threshold does
# two things: it is reported as an anomaly (the cheap nominal/not-nominal gate that decides
# whether the sweep skips the model session), and it files a direct, deduplicated escalation
# — a full root disk kills everything on the box.
#
# POSITIVE CONTROLS FIRST (law-a-regression-test-must-be-seen-to-fail): each "not an anomaly"
# or "no escalation" assertion is preceded by a fixture proving the same field DOES fire when
# it should.
#
# `?` (AN UNREADABLE PROBE) IS TREATED AS AN ANOMALY BUT NEVER ESCALATED
# (law-absence-needs-a-positive-control): a probe that failed is not evidence the disk is
# fine, so it must not take the fast SWEEP:NOMINAL path — but it is not evidence the disk is
# full either, so it must not file an incident on no measurement.
#
# covers: spira/watchtower.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-watchtower-disk-memory.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# FAST MOCK FOR suites.sh status — same trick test-watchtower.sh uses. Every wt() call runs
# the real watchtower.sh, which shells out to suites.sh status; the real one costs seconds.
MOCK_SUITES="$TMP/mock-suites.sh"
printf '#!/usr/bin/env bash\nprintf "  suites in the tree                  0   (0 gated, 0 timed)\\n"\n' \
    > "$MOCK_SUITES"
chmod +x "$MOCK_SUITES"

# STUB df. Controlled by three env vars the caller sets per case:
#   DF_MODE=fail    — exits 1 with no output (an unreadable probe)
#   DF_ROOT_PCT     — the percentage `--output=pcent /` reports (default 10, nominal)
#   DF_WS_PCT       — the percentage `--output=pcent <workspaces>` reports (default 10)
# A plain `df <path>` call (the /tmp check) always reports 5%, which no test here asserts on.
DF_STUB="$TMP/df-stub.sh"
cat > "$DF_STUB" <<'STUBEOF'
#!/usr/bin/env bash
[ "${DF_MODE:-}" = fail ] && exit 1
if [ "$1" = "--output=pcent" ]; then
    if [ "$2" = "/" ]; then pct="${DF_ROOT_PCT:-10}"; else pct="${DF_WS_PCT:-10}"; fi
    printf 'Use%%\n%s%%\n' "$pct"
else
    printf 'Filesystem 1K-blocks Used Available Use%% Mounted\ntmpfs 1 1 1 5%% %s\n' "$1"
fi
STUBEOF
chmod +x "$DF_STUB"

# STUB /proc/meminfo. MEM_AVAIL_KB controls MemAvailable; default is comfortably above the
# 1500MB warn floor. MEMINFO_MODE=missing points SPIRA_MEMINFO at a path that does not exist.
MEMINFO="$TMP/meminfo"
write_meminfo() { printf 'MemTotal:       16000000 kB\nMemAvailable:   %s kB\n' "${1:-8000000}" > "$MEMINFO"; }
write_meminfo

# wt: --show mode — gathers and prints, touches nothing.
wt() {                   # wt [VAR=val ...] -> the snapshot
    local meminfo="$MEMINFO"
    [ "${MEMINFO_MODE:-}" = missing ] && meminfo="$TMP/no-such-meminfo"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_SUITES_SH="$MOCK_SUITES" \
        SPIRA_DF="$DF_STUB" SPIRA_MEMINFO="$meminfo" \
        SPIRA_WORKSPACES="$TMP/workspaces" \
        DF_MODE="${DF_MODE:-}" DF_ROOT_PCT="${DF_ROOT_PCT:-}" DF_WS_PCT="${DF_WS_PCT:-}" \
        "$@" bash "$HERE/watchtower.sh" --show 2>/dev/null
}

# wt_file_multi: full pass (no --show). Captures every incident.sh subject/ref/cause and
# writes the ops prompt so the nominal/not-nominal branch is visible.
wt_file_multi() {   # wt_file_multi [VAR=val ...]
    local mock="$TMP/mock-inc.sh"
    printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$2" >> "%s"\nprintf "%%s\\n" "${SPIRA_INCIDENT_REF:-}" >> "%s"\nprintf "%%s\\n" "${SPIRA_INCIDENT_CAUSE:-}" >> "%s"\ncat > /dev/null\n' \
        "$TMP/inc-subjects" "$TMP/inc-refs" "$TMP/inc-causes" > "$mock"
    chmod +x "$mock"
    local meminfo="$MEMINFO"
    [ "${MEMINFO_MODE:-}" = missing ] && meminfo="$TMP/no-such-meminfo"
    rm -f "$TMP/inc-subjects" "$TMP/inc-refs" "$TMP/inc-causes" "$TMP/ops-prompt"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_SUITES_SH="$MOCK_SUITES" \
        SPIRA_DF="$DF_STUB" SPIRA_MEMINFO="$meminfo" \
        SPIRA_WORKSPACES="$TMP/workspaces" \
        SPIRA_WATCH_PROMPT_FILE="$TMP/ops-prompt" \
        SPIRA_INCIDENT_SH="$mock" \
        DF_MODE="${DF_MODE:-}" DF_ROOT_PCT="${DF_ROOT_PCT:-}" DF_WS_PCT="${DF_WS_PCT:-}" \
        "$@" bash "$HERE/watchtower.sh" 2>/dev/null
}
# A fresh, readable cockpit.env is one of the OTHER nominal conditions (snap_age); every
# case here fixes it at "just now" so the disk/memory field under test is the only thing
# that varies.
fresh() {
    rm -rf "$TMP/run"; mkdir -p "$TMP/run/landstate"
    printf "SP_AT=%s\n" "$(date +%s)" > "$TMP/run/cockpit.env"
}

# ======================================================================================
echo
echo "disk usage on / renders as a vital sign:"
# ======================================================================================
snap="$(DF_ROOT_PCT=37 wt)"
want "the field is present" "disk used on /" "$snap"
want "and carries the reading" "disk used on / (warn >= 90%)   37" "$snap"

# ======================================================================================
echo
echo "disk on / at or above threshold IS an anomaly — T1"
# ======================================================================================
# POSITIVE CONTROL: below threshold, otherwise-nominal state takes the fast path.
fresh
snap="$(DF_ROOT_PCT=10 wt_file_multi)"
want "below threshold: the sweep is nominal" "SWEEP:NOMINAL" "$(cat "$TMP/ops-prompt" 2>/dev/null)"
nowant "below threshold: no disk escalation is filed" "DISK:" "$(cat "$TMP/inc-subjects" 2>/dev/null || echo)"

# THE ANOMALY ITSELF. 95% is above the 90% default threshold.
fresh
DF_ROOT_PCT=95 wt_file_multi
prompt="$(cat "$TMP/ops-prompt" 2>/dev/null || echo)"
nowant "at/above threshold: the sweep is NOT the nominal fast path" "SWEEP:NOMINAL" "$prompt"
want   "at/above threshold: the full sweep still carries the vital signs" "N workers pull" "$prompt"
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo)"
want "and a disk escalation is filed" "DISK: / at 95%" "$subjects"
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo)"
want "with a stable dedup ref" "incident:disk-usage-root" "$refs"

# THE THRESHOLD ITSELF IS CONFIGURABLE — same reading, a different threshold, a different verdict.
fresh
DF_ROOT_PCT=95 wt_file_multi SPIRA_DISK_WARN_PCT=99
want "a wider threshold reads the same 95% as nominal" "SWEEP:NOMINAL" "$(cat "$TMP/ops-prompt" 2>/dev/null)"

# ======================================================================================
echo
echo "disk on the workspaces volume is a second, independent anomaly:"
# ======================================================================================
fresh
DF_ROOT_PCT=10 DF_WS_PCT=92 wt_file_multi
prompt="$(cat "$TMP/ops-prompt" 2>/dev/null || echo)"
nowant "workspaces above threshold alone is not nominal" "SWEEP:NOMINAL" "$prompt"
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo)"
want "and files its own escalation" "DISK: workspaces at 92%" "$subjects"

# ======================================================================================
echo
echo "memory availability renders as a vital sign and anomalies the same way:"
# ======================================================================================
snap="$(wt)"
want "the field is present" "memory available, MB" "$snap"

fresh
write_meminfo 8000000   # ~7.6GB available, comfortably above the 1500MB floor
snap="$(DF_ROOT_PCT=10 wt_file_multi)"
want "plenty of memory: the sweep is nominal" "SWEEP:NOMINAL" "$(cat "$TMP/ops-prompt" 2>/dev/null)"
nowant "and no memory escalation is filed" "MEMORY:" "$(cat "$TMP/inc-subjects" 2>/dev/null || echo)"

fresh
write_meminfo 500000    # ~488MB available, below the 1500MB floor
DF_ROOT_PCT=10 wt_file_multi
prompt="$(cat "$TMP/ops-prompt" 2>/dev/null || echo)"
nowant "low memory: the sweep is NOT the nominal fast path" "SWEEP:NOMINAL" "$prompt"
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo)"
want "and a memory escalation is filed" "MEMORY:" "$subjects"
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo)"
want "with a stable dedup ref" "incident:memory-pressure" "$refs"
write_meminfo 8000000   # restore the default for the cases below

# ======================================================================================
echo
echo "an unreadable probe is an anomaly but is never escalated:"
# ======================================================================================
fresh
DF_MODE=fail wt_file_multi DF_MODE=fail
prompt="$(cat "$TMP/ops-prompt" 2>/dev/null || echo)"
nowant "a failed df probe forces the full sweep, not the fast path" "SWEEP:NOMINAL" "$prompt"
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo)"
nowant "but files no disk escalation on an unread probe" "DISK:" "$subjects"
snap="$(DF_MODE=fail wt DF_MODE=fail)"
want "and the field itself renders ?, never a bare zero" "disk used on / (warn >= 90%)   ?" "$snap"

fresh
MEMINFO_MODE=missing wt_file_multi
prompt="$(cat "$TMP/ops-prompt" 2>/dev/null || echo)"
nowant "a missing meminfo forces the full sweep, not the fast path" "SWEEP:NOMINAL" "$prompt"
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo)"
nowant "but files no memory escalation on an unread probe" "MEMORY:" "$subjects"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
