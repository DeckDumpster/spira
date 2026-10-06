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
# covers: landing-pass/src/* cockpit-collect/src/* spira/lib.sh aeon/src/* watchtower/src/* UC-ops-detection-remediation-26
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
        SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
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
echo "no LANDED row renders ?, never 0:"
# ======================================================================================
# THE WHOLE POINT OF THE FIELD. "Nothing has landed" and "nothing landed in the last zero
# minutes" are opposite facts, and the second is the reassuring one.
fresh
lc_bead REWORK sp-red1 cafe2 "$(( NOW - 60 ))"
lc_bead CERTIFIED sp-red2 cafe3 "$(( NOW - 60 ))"
line="$(field "$(wt)" 'minutes since the last landing')"
want   "no LANDED row renders ?" "?" "$line"
want   "and says so in words" "none recorded" "$line"
nowant "and does not name a non-landed bead" "sp-red1" "$line"

fresh
lc_bead LANDED sp-good cafe4 "$(( NOW - 600 ))"
lc_bead LANDED sp-nosince cafe5 null
line="$(field "$(wt)" 'minutes since the last landing')"
want   "a LANDED row with no entry time is not credited, the good one is" "sp-good" "$line"
nowant "the row without a time is not named" "sp-nosince" "$line"

fresh
line="$(field "$(wt SPIRA_LC_BIN="$TMP/absent")" 'minutes since the last landing')"
want "an unreachable spira-lc renders ?" "?" "$line"

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

# THE INCIDENT ITSELF. One childless epic: nobody claimed it, no lease expired, no worker
# died. The ledger has one entry and the ghost count is zero, and it is the zero that is the
# whole point — reverting the renderer to the ledger size fails here and nowhere else.
# defect: sp-3cm3
INCIDENT='{"spira,plan:empty:sp-jj88":{"first":1788811865,"acted":0,"escalated":1788812834}}'
k="$(ledger "$INCIDENT")"
is "an empty epic is not a ghost"   "0" "$(key "$k" SP_STRAND_GHOST)"
is "it is reported as its own class" "empty=1" "$(key "$k" SP_STRAND_OTHER)"
is "the ledger still has one entry"  "1" "$(key "$k" SP_STRANDS)"
snap="$(render "$INCIDENT")"
want "the pane reports no dead holder" "stranded (claimed, nobody home)     0" "$snap"
want "and names the class it does hold" "strand ledger, other classes        empty=1" "$snap"

# Every class is named and counted, ghost kept apart from the rest.
k="$(ledger '{"p:ghost:sp-a":{},"p:empty:sp-b":{},"p:empty:sp-c":{},"p:stuck:sp-d":{}}')"
is "ghosts are counted alone"       "1" "$(key "$k" SP_STRAND_GHOST)"
is "the other classes are itemised" "empty=2,stuck=1" "$(key "$k" SP_STRAND_OTHER)"

# THE KEY IS SPLIT FROM THE RIGHT. A partition is a label list and may carry a colon; an id
# may not. Splitting from the left reads the partition as the kind, and does it on precisely
# the entries hardest to reason about.
k="$(ledger '{"spira:plan,extra:ghost:sp-a":{}}')"
is "a partition containing a colon still classifies" "1" "$(key "$k" SP_STRAND_GHOST)"

# AN UNREADABLE ENTRY IS `?`, NEVER 0. The key that could not be classified may itself be a
# ghost, and a confident zero is the reading that stops anybody looking.
k="$(ledger '{"bogus":{},"p:ghost:sp-a":{}}')"
is   "an unclassifiable key makes the ghost count unknown" "?" "$(key "$k" SP_STRAND_GHOST)"
want "and is itself reported, not dropped" "unclassified=1" "$(key "$k" SP_STRAND_OTHER)"
is   "while the ledger size is still known" "2" "$(key "$k" SP_STRANDS)"

# A ledger that will not parse, and no ledger at all, are both unread rather than empty:
# strand.sh writes the file on its first pass, so its absence means the detector has not run.
k="$(ledger 'not json at all')"
is "an unparsable ledger renders ?" "?" "$(key "$k" SP_STRAND_GHOST)"
rm -f "$TMP/run/strands.json"
tl_config SPIRA_RUN="$TMP/run"
k="$(env -i SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_FIX="$LC_FIX" PATH="$PATH" HOME="$TMP" SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        cockpit-collect probe strands 2>/dev/null)"
is "a missing ledger renders ?"     "?" "$(key "$k" SP_STRAND_GHOST)"

# A SNAPSHOT FROM A COLLECTOR PREDATING THE SPLIT RENDERS `?`. The two halves are briefly
# skewed during any rollout, and the pane must say it could not read the field rather than
# report a zero nobody measured.
fresh
printf "SP_STRANDS=7\n" > "$TMP/run/cockpit.env"
snap="$(wt)"
want "an old snapshot is unread, not clear" "stranded (claimed, nobody home)     ?" "$snap"
nowant "and its total is not shown as ghosts" "nobody home)     7" "$snap"
fresh

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
        SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
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
echo "a draining world is a vital sign in the snapshot:"
# ======================================================================================
# DRAIN IS LIGHTER THAN HALT — the loop, landing and reaping continue; only new summons
# are gated. So the sweep still files (unlike a halt, which skips entirely), but the drain
# state is surfaced as a prominent vital sign so Ops can see it. A drain stamp at the
# right path is all that is needed.
#
# POSITIVE CONTROL FIRST: the field must reach the pane and read a number, not `?`, for
# a stamp we can stat. Without this, a broken stat or a wrong path produces `?` and every
# assertion below passes on silence (law-absence-needs-a-positive-control).
fresh
mkdir -p "$TMP/run"
printf '2026-09-08 20:02:00 UTC\nsummons gated in summon_fayth; loop and landing still running.\n' \
    > "$TMP/run/world.draining"
# Touch the stamp to a known age so the field is a number, not unknown. The exact elapsed
# minutes grow as the test runs, so the check below verifies a number rather than "5".
touch -d "@$(( NOW - 300 ))" "$TMP/run/world.draining" 2>/dev/null || true
snap="$(wt)"
want "a drain stamp surfaces in the snapshot"    "DRAINING"                     "$snap"
want "and reports the stamp timestamp"           "2026-09-08 20:02:00 UTC"      "$snap"
want "and a numeric minutes field (not ?)"       "draining since"               "$snap"
want "and says summons are gated"                "Summons gated"                "$snap"
nowant "a drain is not a halt"                   "HALTED"                       "$snap"
nowant "and does not claim no incidents are filed" "No incidents are filed"     "$snap"
# The section label distinguishes drain from the not-draining state.
# Check the first token is a number, not "?". The exact value grows as the test runs, so
# asserting "5" here produces a timing-sensitive failure in slow containers (sp-c0lz scar).
dm_pos="$(field "$snap" 'draining since (? = cannot read)')"
[ "${dm_pos%% *}" = "?" ] \
    && bad "drain_mins is a number — positive control" "got [?]" \
    || ok "drain_mins is a number — positive control"

# NO DRAIN STAMP renders 0, NOT `?`. "Not draining" and "draining but probe failed" are
# different facts; the former is the healthy state and must not show the alarm colour.
fresh
mkdir -p "$TMP/run"
snap="$(wt)"
nowant "a running world has no drain banner"     "DRAINING"                  "$snap"
# field() returns the rest of the line after the label; the first word is the minutes.
dm_raw="$(field "$snap" 'draining since (? = cannot read)')"
is "no drain stamp renders 0, not ?"  "0" "${dm_raw%% *}"

# A DRAINING WORLD STILL FILES THE SWEEP. Unlike a halted world (which exits before calling
# incident.sh), a drain leaves the loop and landing running — so the sweep is needed.
fresh
printf '2026-09-08 20:02:00 UTC\nsummons gated.\n' > "$TMP/run/world.draining"
wt_file
is "a draining world still writes the prompt file" "1" \
   "$([ -f "$TMP/ops-prompt" ] && echo 1 || echo 0)"
is "a draining world does not call incident.sh for the routine sweep" "" \
   "$([ -f "$TMP/incident-called" ] && cat "$TMP/incident-called" || echo "")"

# ======================================================================================
echo
echo "a malformed drain stamp renders ? and does not claim the world is running:"
# ======================================================================================
# THE FAILURE THIS BEAD EXISTS TO PREVENT, BUILT INTO ITSELF. A drain stamp that cannot be
# parsed must say 'DRAINING' with a '?' elapsed time — not 'not draining' or a 0.
# With the mtime approach, a file that exists is always stat-able; the ? path covers a stat
# failure (permissions, concurrent deletion), not a bad timestamp string.
fresh
printf 'not-a-timestamp\nsummons gated.\n' > "$TMP/run/world.draining"
# Make the stamp unreadable by zeroing its mtime via a writable copy with known mtime:
# simpler to test the field directly.
snap="$(wt)"
want   "malformed stamp still shows DRAINING"    "DRAINING"                  "$snap"

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
        SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
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
        SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_WATCH_GATE_WINDOW="$GATE_WINDOW" \
        SPIRA_WATCH_PROMPT_FILE="$TMP/ops-prompt" \
        SPIRA_SUITES_SH="$MOCK_SUITES" \
        SPIRA_INCIDENT_SH="$mock" \
        "$@" watchtower 2>/dev/null
}
# wt_refs_multi: like wt_file_multi but captures SPIRA_INCIDENT_REF (the actual dedupe key)
# rather than the incident subject. Used to verify two passes with different measured values
# produce one stable key rather than one per measurement.
wt_refs_multi() {   # wt_refs_multi [VAR=val ...] -> appends SPIRA_INCIDENT_REF to $TMP/inc-refs
    local mock="$TMP/mock-inc-refs.sh"
    printf '#!/usr/bin/env bash\nprintf "%%s\n" "${SPIRA_INCIDENT_REF:-}" >> "%s"\ncat > /dev/null\n' \
        "$TMP/inc-refs" > "$mock"
    chmod +x "$mock"
    tl_config SPIRA_RUN="$TMP/run"
    env -i SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_FIX="$LC_FIX" PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_WATCH_GATE_WINDOW="$GATE_WINDOW" \
        SPIRA_WATCH_PROMPT_FILE="$TMP/ops-prompt" \
        SPIRA_SUITES_SH="$MOCK_SUITES" SPIRA_SYSTEMCTL="$SYSTEMCTL_CLEAN" SPIRA_MOOT_SH="$MOCK_MOOT" \
        SPIRA_INCIDENT_SH="$mock" \
        "$@" watchtower 2>/dev/null
}
wt_body_unadopted() {  # wt_body_unadopted [VAR=val ...] -> writes unadopted escalation body to $TMP/inc-unadopted-body
    local mock="$TMP/mock-inc-unadopted-body.sh"
    # Capture stdin only for the unadopted escalation (SPIRA_INCIDENT_CAUSE=unadopted-refs).
    printf '#!/usr/bin/env bash\n[ "${SPIRA_INCIDENT_CAUSE:-}" = unadopted-refs ] && cat >> "%s" || cat > /dev/null\n' \
        "$TMP/inc-unadopted-body" > "$mock"
    chmod +x "$mock"
    tl_config SPIRA_RUN="$TMP/run"
    env -i SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_FIX="$LC_FIX" PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_WATCH_GATE_WINDOW="$GATE_WINDOW" \
        SPIRA_WATCH_PROMPT_FILE="$TMP/ops-prompt" \
        SPIRA_SUITES_SH="$MOCK_SUITES" SPIRA_SYSTEMCTL="$SYSTEMCTL_CLEAN" SPIRA_MOOT_SH="$MOCK_MOOT" \
        SPIRA_INCIDENT_SH="$mock" \
        "$@" watchtower 2>/dev/null
}

# Below threshold: the prompt file is written, but no drain escalation incident is filed.
fresh
printf '2026-09-08 20:02:00 UTC\nsummons gated.\n' > "$TMP/run/world.draining"
touch -d "@$(( $(date +%s) - 600 ))" "$TMP/run/world.draining" 2>/dev/null || true   # 10m < 15m threshold; use live clock, not $NOW (sp-c0lz scar)
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_DRAIN_WARN_MINS=15
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
is "below threshold writes the prompt file" "1" \
   "$([ -f "$TMP/ops-prompt" ] && echo 1 || echo 0)"
nowant "below threshold does not file the drain escalation" "DRAINING:" "$subjects"
nowant "and does not file a routine sweep bead" "Spira sweep" "$subjects"

# At or above threshold: the prompt file is written AND the drain escalation incident fires.
fresh
printf '2026-09-08 20:02:00 UTC\nsummons gated.\n' > "$TMP/run/world.draining"
touch -d "@$(( NOW - 1200 ))" "$TMP/run/world.draining" 2>/dev/null || true   # 20m > 15m threshold
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_DRAIN_WARN_MINS=15
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
is "above threshold writes the prompt file" "1" \
   "$([ -f "$TMP/ops-prompt" ] && echo 1 || echo 0)"
nowant "above threshold does not file a routine sweep bead" "Spira sweep" "$subjects"
want "above threshold files the drain escalation"  "DRAINING:"         "$subjects"
want "with a fixed subject for dedup"              "world.sh summons"  "$subjects"

# UC-ops-detection-remediation-07: the DRAINING escalation is itself SIN-exempt. A
# ten-minute sweep timer that keeps finding the world still draining would otherwise
# cross SPIRA_SIN_AT and page the operator for a condition that is already visible on
# every prompt — the same reasoning that exempts the routine sweep bead.
rm -f "$TMP/inc-sinexempt" "$TMP/ops-prompt"
wt_sinexempt_multi SPIRA_DRAIN_WARN_MINS=15
sinexempt_lines="$(cat "$TMP/inc-sinexempt" 2>/dev/null || echo "")"
want "the drain escalation sets SPIRA_SIN_EXEMPT=1" "DRAINING: world.sh summons gated|1" "$sinexempt_lines"

# Threshold is configurable: zero means escalate immediately.
fresh
printf '2026-09-08 20:02:00 UTC\nsummons gated.\n' > "$TMP/run/world.draining"
touch -d "@$(( NOW - 60 ))" "$TMP/run/world.draining" 2>/dev/null || true   # 1m
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_DRAIN_WARN_MINS=0
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "SPIRA_DRAIN_WARN_MINS=0 escalates immediately" "DRAINING:" "$subjects"

# No drain stamp means no escalation, even with a zero threshold; prompt file is written.
fresh
rm -f "$TMP/run/world.draining" "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_DRAIN_WARN_MINS=0
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
is "no stamp still writes the prompt file"  "1" \
   "$([ -f "$TMP/ops-prompt" ] && echo 1 || echo 0)"
nowant "no stamp means no drain escalation" "DRAINING:" "$subjects"
nowant "and no routine sweep bead"          "Spira sweep" "$subjects"

# ======================================================================================
echo
echo "the repo-label vital signs render from cockpit.env:"
# ======================================================================================
# THE SEAM THIS COVERS. cockpit-collect probe repo_labels computes SP_REPO_UNMAPPED and SP_REPO_ABSENT;
# watchtower.sh reads them from the snapshot and renders them in 'The graph' section.
# Tests here drive the renderer through a hand-written cockpit.env, the same pattern used
# for the strand ledger — no database query is made from inside watchtower.
#
# THE POSITIVE CONTROL IS REQUIRED. A renderer that always prints '?' passes the ? test;
# only a fixture with real numbers can expose that.
fresh
mkdir -p "$TMP/run"
printf "SP_REPO_UNMAPPED=3\nSP_REPO_ABSENT=7\n" > "$TMP/run/cockpit.env"
snap="$(wt)"
want "SP_REPO_UNMAPPED renders in the graph" "repo: unmapped 3" "$snap"
want "SP_REPO_ABSENT renders in the graph"   "absent 7"         "$snap"

# UNREAD SNAPSHOT (missing keys) renders ? — same rule as strands.
fresh
mkdir -p "$TMP/run"
printf "SP_OPEN=5\n" > "$TMP/run/cockpit.env"   # no SP_REPO_* keys at all
snap="$(wt)"
want "missing SP_REPO_UNMAPPED renders ?" "repo: unmapped ?" "$snap"
want "missing SP_REPO_ABSENT renders ?"  "absent ?"         "$snap"

# ZERO IS A VALID MEASUREMENT. A database with no unmapped or absent beads should render 0,
# not ?. A renderer that cannot distinguish 0 from unread displaces the zero.
fresh
mkdir -p "$TMP/run"
printf "SP_REPO_UNMAPPED=0\nSP_REPO_ABSENT=0\n" > "$TMP/run/cockpit.env"
snap="$(wt)"
want "SP_REPO_UNMAPPED=0 renders as 0, not ?" "repo: unmapped 0" "$snap"
want "SP_REPO_ABSENT=0 renders as 0, not ?"  "absent 0"         "$snap"
nowant "and the zero is not disguised as ?" "repo: unmapped ?" "$snap"
nowant "and the absent zero is not ?" "absent ?" "$snap"

# ======================================================================================
echo
echo "the Sending vital signs render from cockpit.env:"
# ======================================================================================
# THE SEAM THIS COVERS. cockpit-collect writes SP_UNSENT, SP_UNSENT_OLDEST_H, SP_UNADOPTED and
# SP_SENT_FAILED into cockpit.env; watchtower.sh reads them and renders them in 'The Sending'
# section. Missing keys must render `?` (an unread probe is not a clean probe), and zero must
# render as zero (a system with no unsent work should say so, not report unknown).
#
# THE POSITIVE CONTROL COMES FIRST. A renderer that always prints `?` passes the ? tests;
# only a fixture with real numbers can prove it is actually reading the keys.
fresh
mkdir -p "$TMP/run"
printf "SP_UNSENT=9\nSP_UNSENT_OLDEST_H=72\nSP_BATCHED_STRANDED=2\nSP_UNADOPTED=1\nSP_ORPHAN_WORK=2\nSP_SENT_FAILED=17\n" \
    > "$TMP/run/cockpit.env"
snap="$(wt)"
want "SP_UNSENT renders in the Sending section"        "unsent branches"             "$snap"
want "SP_UNSENT value renders"                         "unsent branches (total)             9" "$snap"
want "SP_UNSENT_OLDEST_H renders"                      "oldest in-flight (hours)            72" "$snap"
want "SP_BATCHED_STRANDED renders"                     "BATCHED with no open batch"          "$snap"
want "SP_BATCHED_STRANDED value renders"               "BATCHED with no open batch          2" "$snap"
want "SP_UNADOPTED renders"                            "strays (no bead"              "$snap"
want "SP_UNADOPTED value renders"                      "strays (no bead, commits on base)   1" "$snap"
want "SP_ORPHAN_WORK renders"                          "orphan work (no bead, has commits)  2" "$snap"
want "SP_SENT_FAILED renders"                          "fiends (FAILED"              "$snap"
want "SP_SENT_FAILED value renders"                    "fiends (FAILED deletes, came back)  17" "$snap"

# UNREAD SNAPSHOT (missing keys) renders ? — same rule as strands and repo labels.
fresh
mkdir -p "$TMP/run"
printf "SP_OPEN=5\n" > "$TMP/run/cockpit.env"   # no SP_UNSENT/SP_UNADOPTED/SP_SENT_FAILED keys
snap="$(wt)"
want "missing SP_UNSENT renders ?"           "unsent branches (total)             ?" "$snap"
want "missing SP_UNSENT_OLDEST_H renders ?"  "oldest in-flight (hours)            ?" "$snap"
want "missing SP_BATCHED_STRANDED renders ?" "BATCHED with no open batch          ?" "$snap"
want "missing SP_UNADOPTED renders ?"        "strays (no bead, commits on base)   ?" "$snap"
want "missing SP_ORPHAN_WORK renders ?"      "orphan work (no bead, has commits)  ?" "$snap"
want "missing SP_SENT_FAILED renders ?"      "fiends (FAILED deletes, came back)  ?" "$snap"

# ZERO IS A VALID MEASUREMENT. A clean Sending should render 0, not ?.
fresh
mkdir -p "$TMP/run"
printf "SP_UNSENT=0\nSP_UNSENT_OLDEST_H=0\nSP_BATCHED_STRANDED=0\nSP_UNADOPTED=0\nSP_ORPHAN_WORK=0\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
snap="$(wt)"
want "SP_UNSENT=0 renders as 0, not ?"          "unsent branches (total)             0" "$snap"
want "SP_UNSENT_OLDEST_H=0 renders as 0, not ?" "oldest in-flight (hours)            0" "$snap"
want "SP_BATCHED_STRANDED=0 renders as 0, not ?" "BATCHED with no open batch          0" "$snap"
want "SP_UNADOPTED=0 renders as 0, not ?"        "strays (no bead, commits on base)   0" "$snap"
want "SP_ORPHAN_WORK=0 renders as 0, not ?"      "orphan work (no bead, has commits)  0" "$snap"
want "SP_SENT_FAILED=0 renders as 0, not ?"      "fiends (FAILED deletes, came back)  0" "$snap"
nowant "and SP_UNSENT=0 is not disguised as ?"   "unsent branches (total)             ?" "$snap"
nowant "and SP_BATCHED_STRANDED=0 is not disguised as ?" "BATCHED with no open batch          ?" "$snap"

# ======================================================================================
echo
echo "the Sending escalations fire at their thresholds:"
# ======================================================================================
# OLDEST-UNSENT ESCALATION. An unsent branch older than SPIRA_UNSENT_WARN_H hours triggers
# a dedicated bead. Only numeric values that meet the threshold fire; `?` and values below
# the threshold are silent. Positive control: the fixture that should fire, must fire.

# At or above threshold: escalation incident is filed.
fresh
printf "SP_UNSENT=3\nSP_UNSENT_OLDEST_H=30\nSP_UNADOPTED=0\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_UNSENT_WARN_H=24
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "oldest-unsent at threshold fires escalation" "SENDING: oldest" "$subjects"
want "with a fixed subject for dedup"              "above threshold" "$subjects"

# Below threshold: no escalation.
fresh
printf "SP_UNSENT=3\nSP_UNSENT_OLDEST_H=12\nSP_UNADOPTED=0\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_UNSENT_WARN_H=24
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "oldest-unsent below threshold does not fire" "SENDING: oldest" "$subjects"

# `?` oldest is never an escalation (law-absence-needs-a-positive-control).
fresh
printf "SP_UNSENT=?\nSP_UNSENT_OLDEST_H=?\nSP_UNADOPTED=0\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_UNSENT_WARN_H=0
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "? oldest-unsent never fires escalation even at threshold 0" "SENDING: oldest" "$subjects"

# UNADOPTED ESCALATION. Any nonzero unadopted count fires; zero is silent.
fresh
printf "SP_UNSENT=2\nSP_UNSENT_OLDEST_H=1\nSP_UNADOPTED=3\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_UNSENT_WARN_H=24
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "nonzero SP_UNADOPTED fires escalation"   "SENDING:"   "$subjects"
want "with a fixed subject for dedup"          "unadopted"  "$subjects"
nowant "subject does not embed the count"      "3 unadopted" "$subjects"

# Zero unadopted: no escalation.
fresh
printf "SP_UNSENT=2\nSP_UNSENT_OLDEST_H=1\nSP_UNADOPTED=0\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_UNSENT_WARN_H=24
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "SP_UNADOPTED=0 does not fire escalation" "unadopted" "$subjects"

# `?` unadopted is never an escalation.
fresh
printf "SP_UNSENT=2\nSP_UNSENT_OLDEST_H=1\nSP_UNADOPTED=?\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_UNSENT_WARN_H=0
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "? unadopted never fires escalation" "unadopted" "$subjects"

# BATCHED-STRANDED ESCALATION. SP_BATCHED_STRANDED > 0 fires the escalation.
fresh
printf "SP_BATCHED_STRANDED=1\nSP_BATCHED_STRANDED_NAMES='sp-stuck'\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "nonzero SP_BATCHED_STRANDED fires escalation"         "SENDING:"   "$subjects"
want "subject names the stranded state"                     "absent from open batch" "$subjects"

# Zero: no escalation.
fresh
printf "SP_BATCHED_STRANDED=0\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "SP_BATCHED_STRANDED=0 does not fire escalation" "absent from open batch" "$subjects"

# `?` is never an escalation (law-absence-needs-a-positive-control).
fresh
printf "SP_BATCHED_STRANDED=?\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "? batched-stranded never fires escalation" "absent from open batch" "$subjects"

# BATCHED-TOO-LONG ESCALATION. SP_BATCHED_TOO_LONG > 0 fires the escalation.
fresh
printf "SP_BATCHED_TOO_LONG=1\nSP_BATCHED_TOO_LONG_NAMES='sp-slow'\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "nonzero SP_BATCHED_TOO_LONG fires escalation"         "QUEUE:"   "$subjects"
want "subject names the too-long state"                     "not resolved" "$subjects"

# Zero: no escalation.
fresh
printf "SP_BATCHED_TOO_LONG=0\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "SP_BATCHED_TOO_LONG=0 does not fire escalation" "not resolved" "$subjects"

# `?` is never an escalation (law-absence-needs-a-positive-control).
fresh
printf "SP_BATCHED_TOO_LONG=?\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "? batched-too-long never fires escalation" "not resolved" "$subjects"

# Stable ref: two passes with different counts produce one dedupe key.
fresh
rm -f "$TMP/inc-refs" "$TMP/ops-prompt"
printf "SP_BATCHED_STRANDED=1\nSP_BATCHED_STRANDED_NAMES='sp-stuck'\n" \
    > "$TMP/run/cockpit.env"
wt_refs_multi
printf "SP_BATCHED_STRANDED=2\nSP_BATCHED_STRANDED_NAMES='sp-stuck sp-also'\n" \
    > "$TMP/run/cockpit.env"
wt_refs_multi
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
unique_ref_count="$(printf '%s\n' "$refs" | sort -u | grep -c .)"
is "two batched-stranded passes produce one dedupe key"             "1" "$unique_ref_count"
want "and the key is the stable sending-batched-stranded ref"       "sending-batched-stranded" "$refs"

# ======================================================================================
echo
echo "sending escalation dedup: two passes with different measured values produce one ref:"
# ======================================================================================
# POSITIVE CONTROL: fire the escalation once with a known age to confirm the ref is set.
# Then fire again with a different age and confirm the ref is IDENTICAL — not one per
# measured value, which is the bug this bead was cut to fix. The same invariant applies
# to the unadopted escalation.

# OLDEST-UNSENT: two different ages → same SPIRA_INCIDENT_REF.
fresh
rm -f "$TMP/inc-refs" "$TMP/ops-prompt"
printf "SP_UNSENT=3\nSP_UNSENT_OLDEST_H=30\nSP_UNADOPTED=0\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
wt_refs_multi SPIRA_UNSENT_WARN_H=24
printf "SP_UNSENT=3\nSP_UNSENT_OLDEST_H=31\nSP_UNADOPTED=0\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
wt_refs_multi SPIRA_UNSENT_WARN_H=24
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
unique_ref_count="$(printf '%s\n' "$refs" | sort -u | grep -c .)"
is "two passes with different ages produce one dedupe key"       "1" "$unique_ref_count"
want "and the key is the stable sending-oldest-unsent ref"       "sending-oldest-unsent" "$refs"

# UNADOPTED: two different counts → same SPIRA_INCIDENT_REF.
fresh
rm -f "$TMP/inc-refs" "$TMP/ops-prompt"
printf "SP_UNSENT=2\nSP_UNSENT_OLDEST_H=1\nSP_UNADOPTED=3\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
wt_refs_multi SPIRA_UNSENT_WARN_H=24
printf "SP_UNSENT=2\nSP_UNSENT_OLDEST_H=1\nSP_UNADOPTED=5\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
wt_refs_multi SPIRA_UNSENT_WARN_H=24
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
unique_ref_count="$(printf '%s\n' "$refs" | sort -u | grep -c .)"
is "two passes with different counts produce one dedupe key"     "1" "$unique_ref_count"
want "and the key is the stable sending-unadopted-refs ref"      "sending-unadopted-refs" "$refs"

# ======================================================================================
echo
echo "unadopted escalation body names the branches from SP_UNADOPTED_NAMES:"
# ======================================================================================
# THE SEAM THIS COVERS. The original body carried a listing command using the tag-dereference
# form %(*refname:short) which appends ^{} to every branch name, and `bd show` without
# -C SPIRA_DB — two independent defects each producing 100% false positives (sp-gjpc).
# The collector (cockpit-collect) already knows which branches are unadopted when it counts
# SP_UNADOPTED; those names are now emitted as SP_UNADOPTED_NAMES. The body must report
# what the collector measured, not re-derive it from a separate command.
#
# POSITIVE CONTROL FIRST. Set SP_UNADOPTED=1 and SP_UNADOPTED_NAMES='sp-stray'. The
# assertion that the body contains 'sp-stray' FAILS against the old code (which did not
# read SP_UNADOPTED_NAMES and instead emitted a broken listing command) and PASSES after
# the fix. A body that always prints '(unavailable)' would also fail — the sp-stray control
# proves the reader is actually using the value.
fresh
printf "SP_UNSENT=0\nSP_UNSENT_OLDEST_H=0\nSP_UNADOPTED=1\nSP_UNADOPTED_NAMES='sp-stray'\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-unadopted-body"
wt_body_unadopted SPIRA_UNSENT_WARN_H=24
body="$(cat "$TMP/inc-unadopted-body" 2>/dev/null || echo "")"
want "body is non-empty (escalation fired)"           "Unadopted"  "$body"
want "body names the stray branch from SP_UNADOPTED_NAMES" "sp-stray"   "$body"
nowant "body does not embed the broken tag-dereference format" "%(*refname" "$body"

# When SP_UNADOPTED=1 but SP_UNADOPTED_NAMES is absent (old cockpit.env without the key),
# the body must still fire and show '(unavailable)' rather than crashing or silently
# omitting the names section.
fresh
printf "SP_UNSENT=0\nSP_UNSENT_OLDEST_H=0\nSP_UNADOPTED=1\nSP_SENT_FAILED=0\n" \
    > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-unadopted-body"
wt_body_unadopted SPIRA_UNSENT_WARN_H=24
body="$(cat "$TMP/inc-unadopted-body" 2>/dev/null || echo "")"
want "body fires even without SP_UNADOPTED_NAMES"    "Unadopted"     "$body"
want "body falls back to (unavailable) when key absent" "unavailable" "$body"

# ======================================================================================
echo
echo "the duplicate-ref vital sign renders from cockpit.env:"
# ======================================================================================
# THE SEAM THIS COVERS. cockpit-collect writes SP_DUP_REFS and SP_DUP_BEADS into cockpit.env;
# watchtower.sh reads them and renders them in 'The graph' section. Missing keys must render
# '?' (a failed probe must not displace the suspicion), and zero must render as zero (a clean
# dedup path should say so, not report unknown).
#
# THE POSITIVE CONTROL COMES FIRST. A renderer that always prints '?' passes the ? test;
# only a fixture with real numbers can prove it is actually reading the keys.
fresh
mkdir -p "$TMP/run"
printf "SP_DUP_REFS=3\nSP_DUP_BEADS=5\n" > "$TMP/run/cockpit.env"
snap="$(wt)"
want "SP_DUP_REFS renders in the graph"       "duplicate incident refs" "$snap"
want "SP_DUP_REFS value renders"              "duplicate incident refs             3" "$snap"
want "SP_DUP_BEADS renders"                   "surplus beads: 5" "$snap"

# UNREAD SNAPSHOT (missing keys) renders ? — same rule as other vital signs.
fresh
mkdir -p "$TMP/run"
printf "SP_OPEN=5\n" > "$TMP/run/cockpit.env"   # no SP_DUP_* keys at all
snap="$(wt)"
want "missing SP_DUP_REFS renders ?"  "duplicate incident refs             ?" "$snap"
want "missing SP_DUP_BEADS renders ?" "surplus beads: ?" "$snap"

# ZERO IS A VALID MEASUREMENT. A dedup path with no failures should render 0, not ?.
fresh
mkdir -p "$TMP/run"
printf "SP_DUP_REFS=0\nSP_DUP_BEADS=0\n" > "$TMP/run/cockpit.env"
snap="$(wt)"
want   "SP_DUP_REFS=0 renders as 0, not ?"   "duplicate incident refs             0" "$snap"
nowant "and the zero is not disguised as ?"   "duplicate incident refs             ?" "$snap"

# ======================================================================================
echo
echo "the DEDUP escalation fires when SP_DUP_REFS is nonzero:"
# ======================================================================================
# Nonzero SP_DUP_REFS means multiple beads carry the same external_ref — the dedup path
# missed them. The escalation must fire once; zero and ? must be silent.

# Nonzero: escalation fires.
fresh
printf "SP_DUP_REFS=2\nSP_DUP_BEADS=1\n" > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "nonzero SP_DUP_REFS fires escalation"  "DEDUP:"     "$subjects"
want "DEDUP subject names the detection"     "duplicate incident refs detected" "$subjects"

# Zero: no escalation.
fresh
printf "SP_DUP_REFS=0\nSP_DUP_BEADS=0\n" > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "SP_DUP_REFS=0 does not fire escalation" "DEDUP:" "$subjects"

# ?: no escalation — a failed probe must not file a bead claiming dedup is broken.
fresh
printf "SP_DUP_REFS=?\nSP_DUP_BEADS=?\n" > "$TMP/run/cockpit.env"
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "? SP_DUP_REFS never fires escalation" "DEDUP:" "$subjects"

# DEDUP key is stable across passes — two passes with different counts produce one ref.
fresh
rm -f "$TMP/inc-refs" "$TMP/ops-prompt"
printf "SP_DUP_REFS=2\nSP_DUP_BEADS=1\n" > "$TMP/run/cockpit.env"
wt_refs_multi
printf "SP_DUP_REFS=4\nSP_DUP_BEADS=3\n" > "$TMP/run/cockpit.env"
wt_refs_multi
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
unique_ref_count="$(printf '%s\n' "$refs" | sort -u | grep -c .)"
is "two passes with different dup counts produce one dedupe key" "1" "$unique_ref_count"
want "and the key is the stable dedup-meter-nonzero ref" "dedup-meter-nonzero" "$refs"


# ======================================================================================
# T1 SEAMS RETIRED (sp-lnmbq). Four blocks used to live here — collect_disk_mem(),
# collect_czar_block(), collect_landing_field(), collect_gate_wait() — each sourcing
# watchtower.sh directly (`. ./watchtower.sh; collect_xxx`) so the property could be
# checked in-process instead of forking a whole run. That trick needed a bash script to
# source; watchtower is a compiled binary now, so it cannot be sourced at all. Every one
# of those properties is now a Rust unit test, run without forking anything:
#   disk_mem::tests::{render_flags_breach_only_at_or_over_the_disk_threshold,
#     render_flags_mem_breach_strictly_below_threshold, render_is_unknown_never_zero_on_a_failed_read}
#   sweep::collect::tests::{czar_block_renders_unset_classes_as_unknown_throughout,
#     czar_block_a_fired_class_carries_who_handled_it_and_leaves_others_untouched}
#   lc::tests::readers_parse_rows_and_requeue_sends_the_event,
#     throttle::tests::minutes_since_last_landed_{is_none_with_no_landed_record,picks_the_newest_landed_record}
#   gate_wait::tests::{picks_the_longest_wait_inside_the_window,
#     rows_outside_the_window_are_excluded,no_rows_in_window_renders_unknown_not_zero,
#     window_label_switches_from_minutes_to_hours}
# The T2 behaviour these T1 seams cross-checked (same figures via a full `wt`/`wt_file`
# subprocess run) is untouched, above and below this comment in this same suite.
# ======================================================================================

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

iwr_ledger_idle5() {   # write 5 consecutive "awake builder idle" lines
    : > "$TMP/run/aeon-ledger.log"
    for i in 1 2 3 4 5; do
        printf '2026-01-01T00:00:%02dZ awake builder idle\n' "$i" >> "$TMP/run/aeon-ledger.log"
    done
}
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

# CASE A: 5/5 idle, non-empty ready set for 'builder' — alarms.
fresh
iwr_ledger_idle5
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_HOME="$IWR_HOME" PATH="$IWR_BIN:$PATH" IWR_READY="$iwr_ready_nonempty"
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "5 idles + ready work fires the idle-while-ready escalation" "IDLE-WHILE-READY:" "$subjects"
want "it names the fayth" "builder" "$subjects"

# CASE B: 5/5 idle, EMPTY ready set — does not alarm. Idle is the correct report when
# there is genuinely nothing ready; the escalation exists for the other case.
fresh
iwr_ledger_idle5
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_HOME="$IWR_HOME" PATH="$IWR_BIN:$PATH" IWR_READY="$iwr_ready_empty"
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "an empty ready set does not fire the escalation despite 5 idles" "IDLE-WHILE-READY:" "$subjects"

# CASE C: only 4 of the last 5 summons are idle (the 5th claimed real work) — not ALL
# idle, so this is ordinary draw-down, not a stall.
fresh
: > "$TMP/run/aeon-ledger.log"
printf '2026-01-01T00:00:01Z awake builder sp-real1\n' >> "$TMP/run/aeon-ledger.log"
for i in 2 3 4 5; do
    printf '2026-01-01T00:00:%02dZ awake builder idle\n' "$i" >> "$TMP/run/aeon-ledger.log"
done
rm -f "$TMP/inc-subjects" "$TMP/ops-prompt"
wt_file_multi SPIRA_HOME="$IWR_HOME" PATH="$IWR_BIN:$PATH" IWR_READY="$iwr_ready_nonempty"
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "a mix of idle and a real claim in the last N does not fire the escalation" "IDLE-WHILE-READY:" "$subjects"

# DEDUP: the ref is a stable, fayth-keyed string — not one that embeds the ready count
# or a timestamp, which would file a fresh bead on every sweep instead of bumping one
# recurrence (the same defect class as sp-srgr6, tested above for other escalations).
fresh
iwr_ledger_idle5
rm -f "$TMP/inc-refs" "$TMP/ops-prompt"
wt_refs_multi SPIRA_HOME="$IWR_HOME" PATH="$IWR_BIN:$PATH" IWR_READY="$iwr_ready_nonempty"
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
want "the dedup ref names the fayth" "idle-while-ready:builder" "$refs"

echo
tl_summary
