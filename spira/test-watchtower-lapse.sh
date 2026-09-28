#!/usr/bin/env bash
#
# test-watchtower-lapse.sh — the lapsed-aeon section of the pipeline snapshot.
#
#   ./test-watchtower-lapse.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# sp-a8zy introduced the liveness lease: an aeon whose trace is silent for the full
# lease duration is killed, its work preserved, and a record written under
# $SPIRA_RUN/lapsed/. sp-4ihk plumbs those records into the watchtower snapshot so
# Ops sees them on the next sweep and can classify each into one of four outcomes
# (sop-lapsed-aeon-postmortem). A section that renders empty on a failed read is the
# failure this whole panel is built against — `?`, never silence.
#
# THE POSITIVE CONTROL IS THE ENTIRE FIRST BLOCK. Before asserting that no records
# appear when none exist, this suite plants a record and requires it to reach the
# snapshot. A renderer that drops all records passes the "no new records" test and
# the "unreadable directory" test and every assertion that checks for absence — and
# nothing ever catches it (law-absence-needs-a-positive-control).
#
# THE MARKER SEPARATES NEW FROM OLD. The lapse section shows records since the
# previous sweep. A marker file ($SPIRA_RUN/lapsed.swept) holds the timestamp of
# the last sweep; records with a filename timestamp after the marker are "new". A
# missing marker means all records are new (first sweep). --show does NOT advance
# the marker; only a successful prompt-file write does.
#
# tier: T1
# covers: spira/watchtower.sh spira/lib.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-watchtower-lapse.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# FAST MOCK FOR suites.sh status — see test-watchtower.sh for why: every wt()/wt_file()
# call invokes watchtower.sh, which calls suites.sh status (~3.5s x2 for host-check.sh)
# unless this is set. Missing here made this suite pay that cost on every one of its
# ~20 invocations for no reason this suite's assertions ever needed.
MOCK_SUITES="$TMP/mock-suites.sh"
printf '#!/usr/bin/env bash\nprintf "  suites in the tree                  0   (0 gated, 0 timed)\\n"\n' \
    > "$MOCK_SUITES"
chmod +x "$MOCK_SUITES"

# Run watchtower --show in a clean, minimal environment.
# SPIRA_LAPSED_DIR lets tests override the directory without touching SPIRA_RUN.
wt() {  # wt [VAR=val ...] -> the snapshot
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_LAPSED_DIR="${SPIRA_LAPSED_DIR_OVERRIDE:-$TMP/run/lapsed}" \
        SPIRA_LAPSED_MARKER="${SPIRA_LAPSED_MARKER_OVERRIDE:-$TMP/run/lapsed.swept}" \
        SPIRA_SUITES_SH="$MOCK_SUITES" \
        "$@" bash "$HERE/watchtower.sh" --show 2>/dev/null
}

# wt_file: runs watchtower for real (no --show), writes the prompt file.
# Provides a mock incident.sh so escalation calls are captured but no real beads filed.
wt_file() {  # wt_file [VAR=val ...] -> $TMP/ops-prompt written
    local mock="$TMP/mock-inc.sh"
    printf '#!/usr/bin/env bash\ncat > /dev/null\n' > "$mock"; chmod +x "$mock"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_LAPSED_DIR="${SPIRA_LAPSED_DIR_OVERRIDE:-$TMP/run/lapsed}" \
        SPIRA_LAPSED_MARKER="${SPIRA_LAPSED_MARKER_OVERRIDE:-$TMP/run/lapsed.swept}" \
        SPIRA_WATCH_PROMPT_FILE="$TMP/ops-prompt" \
        SPIRA_INCIDENT_SH="$mock" \
        SPIRA_SUITES_SH="$MOCK_SUITES" \
        "$@" bash "$HERE/watchtower.sh" 2>/dev/null
}

fresh() { rm -rf "$TMP/run"; mkdir -p "$TMP/run/landstate"; }

# Plant a lapse record whose filename timestamp is $1 (YYYYMMDDTHHmmSSZ format)
# and whose bead id is $2. Written in the same format aeon.sh uses. The marker-filtering
# tests below need specific, controllable, out-of-order timestamps that the real writer
# (which always stamps "now") cannot produce, so this hand-written fixture stays for those.
plant_lapse() {
    local ts="$1" bead="$2"
    mkdir -p "$TMP/run/lapsed"
    printf 'bead: %s\nquiet: 600s\nlast: writing output file\nbranch: spira/%s\ntip: abc1234\n' \
        "$bead" "$bead" > "$TMP/run/lapsed/${bead}-${ts}"
}

# ======================================================================================
echo
echo "gap G8 — the real aeon.sh writer feeds the reader, not a hand-written fixture:"
# ======================================================================================
# THE WRITER ITSELF, not a copy of what it emits (mirrors test-watchtower.sh's land_mark
# lift, law-prefer-the-real-dependency). Every assertion above and below this block is
# checked through plant_lapse's hand-formatted record, which reproduces whichever half of
# the aeon.sh/watchtower.sh format the test author remembered. This one proves the two
# programs still agree on the format lib.sh's write_lapse_record actually writes.
fresh
eval "$(sed -n '/^write_lapse_record() *{/,/^}/p' "$HERE/lib.sh")" 2>/dev/null
[ "$(type -t write_lapse_record 2>/dev/null)" = function ] \
    && ok "lib.sh's write_lapse_record could be lifted out and run" \
    || bad "lib.sh's write_lapse_record could be lifted out and run" "no such function — the record format has moved"
if [ "$(type -t write_lapse_record 2>/dev/null)" = function ]; then
    SPIRA_RUN="$TMP/run" write_lapse_record sp-real 600 "writing output file" cafe5 >/dev/null
fi
snap="$(wt)"
want "a record from the real writer reaches the snapshot" "sp-real" "$snap"
want "and carries the real writer's quiet field"          "quiet"   "$snap"

# ======================================================================================
echo
echo "positive control — a planted lapse reaches the snapshot:"
# ======================================================================================
# THE FIRST ASSERTION ESTABLISHES REACHABILITY. Every assertion that follows about
# absence or silence could trivially pass if the section were not rendered at all —
# a renderer that drops every record makes "none since last sweep" true by construction.
# This control must plant a record and require it to appear before any absence can be believed.
fresh
plant_lapse 20260912T000000Z sp-test1
snap="$(wt)"
want "a planted lapse appears in the snapshot"        "sp-test1"              "$snap"
want "and the section heading is present"             "Lapsed aeons"          "$snap"
want "and the record's quiet field is present"        "quiet"                 "$snap"
want "and the record's last-action field is present"  "writing output file"   "$snap"
want "and the bead id appears in the record"          "bead: sp-test1"        "$snap"

# ======================================================================================
echo
echo "no lapsed directory → none since last sweep, not ?:"
# ======================================================================================
# A missing directory means no lapses have ever been recorded — a valid and expected
# state. It must render as an explicit "none" rather than ? (which signals a read failure).
fresh
# No $TMP/run/lapsed directory at all.
snap="$(wt)"
want "no directory renders 'none since last sweep'" "none since last sweep" "$snap"
nowant "no directory does not render ?"             "?"                     "$(printf '%s\n' "$snap" | grep -i 'lapsed aeons' -A3 || true)"

# ======================================================================================
echo
echo "existing directory with no new records → none since last sweep:"
# ======================================================================================
fresh
mkdir -p "$TMP/run/lapsed"
# Directory exists but is empty.
snap="$(wt)"
want "empty directory renders 'none since last sweep'" "none since last sweep" "$snap"

# ======================================================================================
echo
echo "the marker filters old records — only new ones appear:"
# ======================================================================================
# A marker file at $SPIRA_RUN/lapsed.swept holds the timestamp of the previous sweep.
# Records whose filename timestamp is at or before the marker are already-seen and
# must not appear in the current snapshot.
fresh
plant_lapse 20260912T100000Z sp-old
plant_lapse 20260912T120000Z sp-new
# Marker at 20260912T110000Z: sp-old (10:00) should be filtered; sp-new (12:00) appears.
printf '20260912T110000Z\n' > "$TMP/run/lapsed.swept"
snap="$(wt)"
want   "a record newer than the marker appears"      "sp-new"    "$snap"
nowant "a record at or before the marker is filtered" "sp-old"   "$snap"
want   "and the section heading is still present"    "Lapsed aeons" "$snap"

# ======================================================================================
echo
echo "no marker means all records are new (first sweep):"
# ======================================================================================
fresh
plant_lapse 20260912T100000Z sp-first
plant_lapse 20260912T110000Z sp-second
rm -f "$TMP/run/lapsed.swept"
snap="$(wt)"
want "without a marker, all records appear" "sp-first"  "$snap"
want "both records appear"                 "sp-second" "$snap"

# ======================================================================================
echo
echo "the section carries a count, not just the records:"
# ======================================================================================
# The section header says how many lapses appear. One lapse and zero lapses are
# distinguishable at a glance, without reading the body.
fresh
plant_lapse 20260912T100000Z sp-one
snap="$(wt)"
want "one lapse names the count in the section" "1 lapse"   "$snap"

fresh
snap="$(wt)"
want "no lapses says none since last sweep"     "none since" "$snap"

# ======================================================================================
echo
echo "an unreadable directory renders ? rather than none:"
# ======================================================================================
# ? means the probe FAILED to read. It is different from "none since last sweep" which
# means the probe SUCCEEDED and found nothing. Conflating them is the failure mode the
# whole ? convention exists to expose (law-absence-needs-a-positive-control).
# We use SPIRA_LAPSED_DIR_OVERRIDE to point at a non-directory path.
fresh
touch "$TMP/run/not-a-dir"
snap="$(SPIRA_LAPSED_DIR_OVERRIDE="$TMP/run/not-a-dir" wt)"
lapsed_lines="$(printf '%s\n' "$snap" | grep -A5 'Lapsed aeons' || true)"
want "an unreadable directory renders ?" "?" "$lapsed_lines"
nowant "and does not claim none since"   "none since last sweep" "$lapsed_lines"

# ======================================================================================
echo
echo "the marker advances after a successful prompt-file write (not on --show):"
# ======================================================================================
# --show reads and renders but touches nothing. The marker must not advance in --show
# mode, or a test that calls wt() (which uses --show) would consume records before
# the sweep writes its prompt file.
fresh
plant_lapse 20260912T100000Z sp-x
rm -f "$TMP/run/lapsed.swept"
# --show: marker must NOT be created.
wt > /dev/null
is "--show does not create the marker" "" \
   "$([ -f "$TMP/run/lapsed.swept" ] && cat "$TMP/run/lapsed.swept" || echo "")"

# wt_file: real write path; marker MUST advance.
fresh
plant_lapse 20260912T100000Z sp-y
rm -f "$TMP/run/lapsed.swept"
wt_file
marker="$(cat "$TMP/run/lapsed.swept" 2>/dev/null || echo "")"
[ -n "$marker" ] && ok "wt_file advances the marker" \
                  || bad "wt_file advances the marker" "marker is empty or missing"
# The next wt_file call with the same records finds none (already marked).
snap="$(wt)"
nowant "after the marker advances, the old record is not shown" "sp-y" "$snap"
want   "and the section renders none since last sweep"   "none since last sweep" "$snap"

# D12: the "every section present alongside Lapsed aeons" loop that used to live here
# merged into test-watchtower.sh's own whole-snapshot render test (the two differed only
# in 'The graph' vs 'Lapsed aeons' — one snapshot now plants both fixtures and checks all
# seven sections in one pass).

# ======================================================================================
echo
echo "two lapses of one class within one sweep period appear as two records:"
# ======================================================================================
# Deduplication by class is Ops's job when filing beads/statutes. The snapshot shows
# all lapses; a single sweep sees all of them so Ops can decide if they are the same class.
fresh
plant_lapse 20260912T100000Z sp-aa
plant_lapse 20260912T110000Z sp-bb
snap="$(wt)"
want "first lapse appears"  "sp-aa" "$snap"
want "second lapse appears" "sp-bb" "$snap"

echo
tl_summary
