#!/usr/bin/env bash
#
# test-watchtower.sh — the pipeline's detector can read the far end of its own queue.
#
#   ./test-watchtower.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# The watchtower is the mechanism law-detection-outranks-rejection put in FRONT of the gate:
# it exists so a queue that has stopped moving is seen. Its headline field — minutes since
# the last landing — was structurally unreadable from the day it shipped, and rendered
# `?  (last: none recorded)` on every sweep ever filed, including passes where six beads had
# landed in the previous twelve minutes. Three Ops sessions were woken by it and all three
# closed with the same verdict: the pipeline is moving, the instrument lied.
#
# That is the failure mode a detector has and a gate does not. A gate that breaks refuses
# good work, loudly. A detector that breaks reports the stall and the healthy case
# IDENTICALLY, and the reassuring reading is the one it gives — which is why a suite for it
# earns its place beside the pipeline checks despite being neither a fence nor a soak.
#
# THE LANDING FIELD READS spira-lc, so the fixture is a stand-in spira-lc (testlib's
# lc_fix_init) answering from rows this suite plants, and `wt` points SPIRA_LC_BIN at it.
#
# A ROW-LESS spira-lc AND AN UNREACHABLE ONE BOTH RENDER ?, never 0, and each case plants a
# LANDED row beside it so the absence is read through a reader that demonstrably finds one.
#
# SPIRA_RUN AND SPIRA_WATCH_GATE_WINDOW ARE PINNED TO NON-DEFAULTS, and the program is run in
# an empty environment (law-gates-run-in-a-clean-environment). A suite that inherited a real
# spira.conf would assert against one box's lifecycle rows, and asserting against the
# shipped six-hour window would pass just as well if the code had the literal written in,
# which is the thing the key exists to prevent.
#
# defect: sp-86q8
# tier: T1
# covers: landing-pass/src/* cockpit-collect/src/* spira/lib.sh aeon/src/* watchtower/src/* UC-ops-detection-remediation-26 UC-ops-detection-remediation-27
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-watchtower"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
NOW="$(date +%s)"
GATE_WINDOW=3600         # deliberately not the shipped 21600

# land_mark (lib.sh) is now a one-line shim onto `landing-pass mark` (sp-cnnt6, "wave
# 4.16"), which reads $SPIRA_RUN alone rather than a lib.sh seam — exported here so the
# extraction below still reaches the real writer. `wt()`/`ledger()` set their own SPIRA_RUN
# inside `env -i`, which overrides this, so the two never disagree.
export SPIRA_RUN="$TMP/run"

# FAST MOCK FOR suites.sh status. suites.sh status calls host-check.sh twice (~3.5s each),
# and every wt() / wt_file_multi() call invokes watchtower which calls suites.sh status.
# At ~40 total invocations that is ~280s before any actual test logic runs. The mock returns
# a minimal but structurally valid block in under 1ms so the suite completes in a few minutes
# instead of running into the per-suite gate timeout. It still says "suites in the tree"
# so the assertion at line 350 ("carries its cheap figures") passes.
MOCK_SUITES="$TMP/mock-suites.sh"
printf '#!/usr/bin/env bash\nprintf "  suites in the tree                  0   (0 gated, 0 timed)\\n"\n' \
    > "$MOCK_SUITES"
chmod +x "$MOCK_SUITES"
# moot.sh --apply runs on every filing pass and nothing here asserts on it: a no-op stands in.
MOCK_MOOT="$TMP/mock-moot.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_MOOT"
chmod +x "$MOCK_MOOT"

# A HERMETIC DISK/MEMORY READING, DEFAULT FOR EVERY wt()/wt_file() CALL
# (law-gates-run-in-a-clean-environment). Without this, every assertion about the nominal
# path is silently coupled to this host's real root-disk usage and real free memory — a
# box that happens to be 90% full on the day this runs would fail tests that have nothing
# to do with disk. (Since sp-gypjk conf.sh keeps the caller's PATH first and only appends
# SPIRA_PATH, so the stub dir is also put first on PATH wherever PATH is passed through.)
# Historically: conf.sh rebuilt PATH from SPIRA_PATH plus a fixed suffix and discarded
# whatever PATH was inherited, so prepending to $PATH here is invisible by the time df
# runs — SPIRA_PATH is the only seam that lands. The stub reports WT_DISK_PCT (default 12)
# so a test wanting the anomaly path sets that var; SPIRA_MEMINFO_PATH is a plain path
# override a test can replace directly, being later in the env invocation.
DF_CLEAN="$TMP/df-clean"; mkdir -p "$DF_CLEAN"
cat > "$DF_CLEAN/df" <<'STUB'
#!/bin/sh
if [ "$1" = "--output=pcent" ] && [ "$2" = "/" ]; then
    printf 'Use%%\n %s%%\n' "${WT_DISK_PCT:-12}"
else
    /usr/bin/df "$@"
fi
STUB
chmod +x "$DF_CLEAN/df"
MEMINFO_CLEAN="$TMP/meminfo-clean"
printf 'MemAvailable:   16000000 kB\n' > "$MEMINFO_CLEAN"

# A CLEAN SYSTEMCTL, DEFAULT FOR EVERY wt()/wt_file()/wt_file_multi() CALL — same reasoning
# as DF_CLEAN above. Without a stub, `systemctl --user list-units --state=failed` runs for
# real and this suite's idea of "nominal" starts depending on whatever units are actually
# failed on the box running it (sp-niqjl's own failed-units check is not what this suite
# covers — that is test-watchtower-failed-units.sh).
SYSTEMCTL_CLEAN="$TMP/systemctl-clean"
printf '#!/bin/sh\nexit 0\n' > "$SYSTEMCTL_CLEAN"
chmod +x "$SYSTEMCTL_CLEAN"

# The program under test, in an environment holding nothing but what it needs. `--show`
# gathers and prints and touches nothing, so nothing here can reach a database or file a bead.
wt() {                   # wt [VAR=val ...] -> the snapshot
    tl_config SPIRA_RUN="$TMP/run" SPIRA_PATH="$DF_CLEAN"
    env -i SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_FIX="$LC_FIX" PATH="$DF_CLEAN:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" \
        SPIRA_WATCH_GATE_WINDOW="$GATE_WINDOW" \
        SPIRA_SUITES_SH="$MOCK_SUITES" \
        SPIRA_MEMINFO_PATH="$MEMINFO_CLEAN" SPIRA_SYSTEMCTL="$SYSTEMCTL_CLEAN" \
        "$@" watchtower --show 2>/dev/null
}
# THE LABEL IS MATCHED LITERALLY, never with a `.*`. The value is separated from the label
# by run of spaces, so a greedy wildcard in the label happily swallows the value too and
# every assertion below then reads the branch name and reports the wait as missing.
field() {                # field <snapshot> <label> -> the rest of that line
    printf '%s\n' "$1" | sed -n "s/^  $2  *//p" | head -1
}
gate_field() {           # gate_field <snapshot> [<window label>]
    field "$1" "longest gate wait, ${2:-last 1h}"
}
fresh() { rm -rf "$TMP/run"; mkdir -p "$TMP/run"; lc_fix_init "$TMP/lc"; }

# ======================================================================================
echo
echo "the landing field reads the newest LANDED bead from spira-lc:"
# ======================================================================================
fresh
lc_bead LANDED sp-old cafe0 "$(( NOW - 3000 ))"
lc_bead LANDED sp-land cafe1 "$(( NOW - 600 ))"
lc_bead CERTIFIED sp-cert cafe9 "$(( NOW - 60 ))"
snap="$(wt)"
line="$(field "$snap" 'minutes since the last landing')"
nowant "a LANDED row renders a number, not ?" "?" "$line"
want   "and names the bead that landed most recently" "sp-land" "$line"
nowant "and not an older landing" "sp-old" "$line"
nowant "nor a bead that has not landed" "sp-cert" "$line"

# ======================================================================================
echo
echo "the gate-wait field reads gate.log through the compiled binary:"
# ======================================================================================
fresh
printf '%s gate sp-slowgate waited=90s ok\n' "$(date -u -d "@$(( NOW - 100 ))" +%Y-%m-%dT%H:%M:%SZ)" > "$TMP/run/gate.log"
line="$(gate_field "$(wt)")"
nowant "a gate.log row in the window renders a wait, not ?" "?" "$line"
want   "and names the branch that waited" "sp-slowgate" "$line"

# ======================================================================================
echo
echo "the strand ledger is reported by class, not by size:"
# ======================================================================================
# strands.json holds EVERY disposition strand.sh classifies — ghost, empty, starved, stuck,
# cycle — and only `ghost` is the labelled failure of a claimed bead whose holder is gone.
# The watchtower rendered the ledger's SIZE under that name, so a childless epic reported as
# a dead worker and a sweep spent four commands hunting for a holder that never existed.
#
# THE FIXTURE GOES THROUGH THE REAL COLLECTOR (law-prefer-the-real-dependency). The classifier
# under test is `cockpit-collect probe strands`, the same function probe calls, and its output IS the
# snapshot the renderer then reads — so the seam between the two programs is exercised rather
# than imagined. Writing a cockpit.env by hand here would assert against whichever key names
# the test author remembered, which is exactly the drift the split was made to stop.
ledger() {               # ledger <json> -> the collector's keys for that ledger
    mkdir -p "$TMP/run"
    printf '%s' "$1" > "$TMP/run/strands.json"
    tl_config SPIRA_RUN="$TMP/run"
    env -i SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_FIX="$LC_FIX" PATH="$PATH" HOME="$TMP" SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        cockpit-collect probe strands 2>/dev/null
}
key() {                  # key <keys> <name> -> its value
    printf '%s\n' "$1" | sed -n "s/^$2=//p" | head -1
}
render() {               # render <json> -> the watchtower snapshot over that ledger
    local keys; keys="$(ledger "$1")"
    printf '%s\n' "$keys" > "$TMP/run/cockpit.env"
    wt
}

# THE POSITIVE CONTROL FIRST: a ghost really is counted as a ghost, and reaches the pane. If
# this ever goes quiet, every assertion below is a matcher that finds nothing being read as
# a system with nothing wrong (law-absence-needs-a-positive-control).
k="$(ledger '{"spira,plan:ghost:sp-a":{},"spira,plan:ghost:sp-b":{}}')"
is "two ghosts count as two"        "2" "$(key "$k" SP_STRAND_GHOST)"
is "and nothing else is reported"   "none" "$(key "$k" SP_STRAND_OTHER)"
want "and the pane says so" "stranded (claimed, nobody home)     2" \
     "$(render '{"spira,plan:ghost:sp-a":{},"spira,plan:ghost:sp-b":{}}')"

# ======================================================================================
echo
echo "the snapshot still renders as a whole:"
# ======================================================================================
# A CHEAP END-TO-END, because every assertion above reads one line out of a document that a
# `set -u` failure would truncate silently — leaving a `sed` that matches nothing and a
# handful of assertions that never ran. Plants both a landed record and a lapse record so
# this one snapshot exercises every section — D12: this used to be two near-identical loops,
# here and in test-watchtower-lapse.sh, differing only in 'The graph' vs 'Lapsed aeons'.
fresh
lc_bead LANDED sp-whole cafe5 "$(( NOW - 600 ))"
if [ "$(type -t write_lapse_record 2>/dev/null)" = function ]; then
    SPIRA_RUN="$TMP/run" write_lapse_record sp-whole-lapsed 600 "writing output file" cafe5 >/dev/null
fi
snap="$(wt)"
for section in 'The far end' 'The Sending' 'The workers' 'The graph' 'Lapsed aeons' 'The menu' 'Can this snapshot be believed'; do
    want "the snapshot still carries: $section" "$section" "$snap"
done
want "and still warns that ? is not a zero" "never treat it as a zero" "$snap"
want "and ends with the ops directive"       "Your task"               "$snap"

# ======================================================================================
echo
echo "the sweep NAMES the scans rather than running them:"
# ======================================================================================
# THE DIVISION IS THE POINT AND IT IS LOAD-BEARING. A several-minute suite run inside this
# program would make the detector the thing that is down during an outage, and would push a
# ten-minute cadence past the interval that produces it. So the sweep carries suites.sh
# status's cheap figures, not its own run of anything, and the Ops session spends its own
# budget on it.
want "and carries its cheap figures, not its output" "suites in the tree" "$snap"
# It must not have RUN anything: `--show` touches nothing, and a suite executed here would
# have written a result under the scratch runtime directory.
is "and the sweep ran no suite of its own" "0" \
   "$(find "$TMP/run" -name '*.result' 2>/dev/null | wc -l)"


# ======================================================================================
echo
echo "a halted world shows the halt in --show and files nothing:"
# ======================================================================================
# THE SEAM THIS COVERS. cockpit/health.sh reads world.halted directly to avoid a stale
# snapshot masking a halt; watchtower must do the same. The positive control here is
# the running path — asserting only the halted case would leave the alarm the watchtower
# exists for untested (law-absence-needs-a-positive-control).
#
# wt_file: runs watchtower WITHOUT --show. Injects a mock incident.sh (for escalation
# checks) and a dedicated SPIRA_WATCH_PROMPT_FILE so prompt writes are visible without
# touching the real runtime. SPIRA_INCIDENT_SH carries the incident.sh override.
wt_file() {   # wt_file [VAR=val ...] -> $TMP/ops-prompt written; $TMP/incident-called if incident.sh fires
    local mock="$TMP/mock-inc.sh"
    printf '#!/usr/bin/env bash\nprintf called > "%s"\ncat > /dev/null\n' \
        "$TMP/incident-called" > "$mock"
    chmod +x "$mock"
    rm -f "$TMP/incident-called" "$TMP/ops-prompt"
    tl_config SPIRA_RUN="$TMP/run" SPIRA_PATH="$DF_CLEAN"
    env -i SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_FIX="$LC_FIX" PATH="$DF_CLEAN:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" \
        SPIRA_WATCH_GATE_WINDOW="$GATE_WINDOW" \
        SPIRA_WATCH_PROMPT_FILE="$TMP/ops-prompt" \
        SPIRA_INCIDENT_SH="$mock" SPIRA_SUITES_SH="$MOCK_SUITES" SPIRA_MOOT_SH="$MOCK_MOOT" \
        SPIRA_MEMINFO_PATH="$MEMINFO_CLEAN" SPIRA_SYSTEMCTL="$SYSTEMCTL_CLEAN" \
        "$@" watchtower 2>/dev/null
}

fresh
printf '2026-09-08T01:23:45Z\nwhy: deliberate halt for testing\n' > "$TMP/run/world.halted"
snap="$(wt)"
want "a halted --show names the halt"       "HALTED"                    "$snap"
want "and shows the halt timestamp"         "2026-09-08T01:23:45Z"      "$snap"
want "and shows the reason"                 "deliberate halt for testing" "$snap"
want "and says no incidents are filed"      "No incidents are filed"    "$snap"
# THE BODY IS STILL PRESENT. The human watching the pane needs to see why the numbers
# look bad, not just that the world is halted; hiding the body would make a quiet
# pane look the same whether the halt is in force or the detector is broken.
want "and the body is still present"        "N workers pull"            "$snap"

fresh
printf '2026-09-08T01:23:45Z\nwhy: deliberate halt for testing\n' > "$TMP/run/world.halted"
wt_file
is "a halted world does not write the prompt file" "" \
   "$([ -f "$TMP/ops-prompt" ] && echo written || echo "")"
is "a halted world does not call incident.sh" "" \
   "$([ -f "$TMP/incident-called" ] && cat "$TMP/incident-called" || echo "")"

# ======================================================================================
echo
echo "a running world carries no halt banner, and does file:"
# ======================================================================================
fresh
# No world.halted stamp.
snap="$(wt)"
nowant "a running world has no halt banner"      "HALTED"              "$snap"
nowant "and no 'no incidents' line"              "No incidents are filed" "$snap"

fresh
wt_file
is "a running world writes the prompt file" "1" \
   "$([ -f "$TMP/ops-prompt" ] && echo 1 || echo 0)"
want "and the prompt contains the snapshot" "N workers pull" \
   "$(cat "$TMP/ops-prompt" 2>/dev/null || echo "")"
is "a running world does not call incident.sh for the routine sweep" "" \
   "$([ -f "$TMP/incident-called" ] && cat "$TMP/incident-called" || echo "")"

# ======================================================================================
echo
echo "a draining world still files the sweep:"
# ======================================================================================
# Unlike a halt, a drain leaves the loop and landing running, so the sweep is needed.
fresh
printf '2026-09-08 20:02:00 UTC\nsummons gated.\n' > "$TMP/run/world.draining"
wt_file
is "a draining world still writes the prompt file" "1" \
   "$([ -f "$TMP/ops-prompt" ] && echo 1 || echo 0)"
is "a draining world does not call incident.sh for the routine sweep" "" \
   "$([ -f "$TMP/incident-called" ] && cat "$TMP/incident-called" || echo "")"

# ======================================================================================
echo
echo "drain escalation: a bead is cut once the threshold is exceeded:"
# ======================================================================================
# wt_file_multi: runs watchtower without --show, collecting ALL incident.sh calls.
# incident.sh is invoked as: incident.sh file "<subject>" -
# so $1=file, $2=subject, $3=-. The mock appends $2 to a file so we can inspect subjects.
# Also writes the prompt file to $TMP/ops-prompt (via SPIRA_WATCH_PROMPT_FILE).
wt_file_multi() {   # wt_file_multi [VAR=val ...] -> appends incident subjects to $TMP/inc-subjects; writes $TMP/ops-prompt
    local mock="$TMP/mock-inc-multi.sh"
    printf '#!/usr/bin/env bash\nprintf "%%s\n" "$2" >> "%s"\ncat > /dev/null\n' \
        "$TMP/inc-subjects" > "$mock"
    chmod +x "$mock"
    tl_config SPIRA_RUN="$TMP/run"
    env -i SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_FIX="$LC_FIX" PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" \
        SPIRA_WATCH_GATE_WINDOW="$GATE_WINDOW" \
        SPIRA_WATCH_PROMPT_FILE="$TMP/ops-prompt" \
        SPIRA_SUITES_SH="$MOCK_SUITES" SPIRA_SYSTEMCTL="$SYSTEMCTL_CLEAN" SPIRA_MOOT_SH="$MOCK_MOOT" \
        SPIRA_INCIDENT_SH="$mock" \
        "$@" watchtower 2>/dev/null
}
# wt_sinexempt_multi: like wt_file_multi but captures SPIRA_SIN_EXEMPT (UC-ops-detection-
# remediation-07) alongside the subject, one "<subject>|<SPIRA_SIN_EXEMPT>" line per
# incident.sh call — proving the DRAINING escalation cannot itself become a Sin, not just
# that incident.sh's own exemption plumbing works (that half is test-sin-exempt.sh's job).
wt_sinexempt_multi() {   # wt_sinexempt_multi [VAR=val ...] -> appends to $TMP/inc-sinexempt
    local mock="$TMP/mock-inc-sinexempt.sh"
    printf '#!/usr/bin/env bash\nprintf "%%s|%%s\n" "$2" "${SPIRA_SIN_EXEMPT:-}" >> "%s"\ncat > /dev/null\n' \
        "$TMP/inc-sinexempt" > "$mock"
    chmod +x "$mock"
    tl_config SPIRA_RUN="$TMP/run"
    env -i SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_FIX="$LC_FIX" PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" \
        SPIRA_WATCH_GATE_WINDOW="$GATE_WINDOW" \
        SPIRA_WATCH_PROMPT_FILE="$TMP/ops-prompt" \
        SPIRA_SUITES_SH="$MOCK_SUITES" SPIRA_SYSTEMCTL="$SYSTEMCTL_CLEAN" SPIRA_MOOT_SH="$MOCK_MOOT" \
        SPIRA_INCIDENT_SH="$mock" \
        "$@" watchtower 2>/dev/null
}
# The threshold logic is escalate::tests; what only this seam proves is that the compiled
# binary carries SPIRA_SIN_EXEMPT through to incident.sh, so the escalation cannot itself
# become a Sin.
fresh
printf '2026-09-08 20:02:00 UTC\nsummons gated.\n' > "$TMP/run/world.draining"
touch -d "@$(( NOW - 1200 ))" "$TMP/run/world.draining" 2>/dev/null || true
rm -f "$TMP/inc-sinexempt" "$TMP/ops-prompt"
wt_sinexempt_multi SPIRA_DRAIN_WARN_MINS=15
want "the drain escalation sets SPIRA_SIN_EXEMPT=1" "DRAINING: world.sh summons gated|1" \
     "$(cat "$TMP/inc-sinexempt" 2>/dev/null || echo "")"

# ======================================================================================
echo
echo "idle-while-ready escalation (sp-o4trx): a fayth with ready work whose last N"
echo "summons ALL ledgered idle is a claim-error masquerading as an empty queue:"
# ======================================================================================
# A FIXTURE CHAMBER, NOT THE REAL ONE. bulk_ready_by_fayth reads FAYTH_LABELS from
# $SPIRA_HOME/chamber/<fayth>.fayth and calls $SPIRA_HOME/ready-bucket.py; a minimal
# fixture pins FAYTH_LABELS to a literal (never the shipped default) and avoids this
# suite depending on the real builder.fayth's own label composition.
IWR_HOME="$TMP/iwr-home"; mkdir -p "$IWR_HOME/chamber"
# SPIRA_CHAMBER EXPLICITLY: the complete fixture declares a fixed chamber path of its own
# now, no longer derived from SPIRA_HOME when unset.
tl_config SPIRA_FAYTHS=builder SPIRA_CHAMBER="$IWR_HOME/chamber"
cp "$HERE/ready-bucket.py" "$IWR_HOME/"
cat > "$IWR_HOME/chamber/builder.fayth" <<'FAYTH'
FAYTH_NAME=builder
FAYTH_LABELS=plan
FAYTH_EXCLUDE_LABELS=""
FAYTH
# READY WORK IS spira-claim's ANSWER (sp-860zj: bulk-ready-by-fayth asks the lifecycle for READY
# rows and bd for their labels; the old SPIRA_READY_SNAPSHOT is read by nothing). Readiness itself
# is spira-claim's subject, tested there; here a stub first on PATH answers bulk-ready-by-fayth
# from a fixture file and hands every other verb to the real spira-claim. The watchtower's probe
# sources $SPIRA_HOME/lib.sh, so the fixture home gets the real one.
printf '. "%s/lib.sh"\n' "$HERE" > "$IWR_HOME/lib.sh"
ln -sf "$HERE/conf.d" "$IWR_HOME/conf.d"
IWR_BIN="$TMP/iwr-bin"; mkdir -p "$IWR_BIN"
_iwr_real_claim="$(command -v spira-claim)"
cat > "$IWR_BIN/spira-claim" <<STUB
#!/usr/bin/env bash
[ "\${1:-}" = bulk-ready-by-fayth ] && { cat "\$IWR_READY" 2>/dev/null; exit 0; }
exec "$_iwr_real_claim" "\$@"
STUB
chmod +x "$IWR_BIN/spira-claim"
iwr_ready_nonempty="$TMP/iwr-ready-nonempty.txt"
printf 'builder 1\n' > "$iwr_ready_nonempty"
iwr_ready_empty="$TMP/iwr-ready-empty.txt"
: > "$iwr_ready_empty"

# Five idle summons of a fayth that has ready work is the escalation; the other cases are
# collect::tests.
fresh
: > "$TMP/run/aeon-ledger.log"
for i in 1 2 3 4 5; do
    printf '2026-01-01T00:00:%02dZ awake builder idle\n' "$i" >> "$TMP/run/aeon-ledger.log"
done
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_HOME="$IWR_HOME" PATH="$IWR_BIN:$PATH" IWR_READY="$iwr_ready_nonempty"
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "5 idles + ready work fires the idle-while-ready escalation" "IDLE-WHILE-READY:" "$subjects"
want "it names the fayth" "builder" "$subjects"

echo
tl_summary
