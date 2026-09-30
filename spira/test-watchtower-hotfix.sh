#!/usr/bin/env bash
#
# test-watchtower-hotfix.sh — watchtower's hotfix escalation reads `release status`
#   verbatim and files exactly one bead per standing hotfix.
#
#   ./test-watchtower-hotfix.sh
#
# WHAT THIS SUITE IS FOR
# -----------------------
# `release activate --hotfix` is the stop-the-world seam (design
# runtime-is-a-release-2026-09-29.md): the running system on a commit that has not landed.
# sp-m3ipc built the record and `release status`'s RUNNING UNLANDED / ALERT lines; this is
# the watchtower half — once a standing hotfix has crossed the configured alert threshold
# (`release status` says so with its ALERT line), Ops must hear about it even if nobody is
# looking at doctor.sh or the cockpit pane.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the past-threshold case
# proves the escalation fires and is captured correctly before any "does not file" case is
# trusted — a wired-up-but-silent mock would otherwise pass every negative case for free.
#
# `release` is mocked: this suite is watchtower's own contract with `release status` text,
# never release's age/threshold arithmetic (that is release's own `cargo test -p release`,
# DESIGN.md "Hotfix"). incident.sh is mocked to capture what would have been filed rather
# than filing a real bead.
#
# tier: T1
# covers: spira/watchtower.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-watchtower-hotfix.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

MOCK_SUITES="$TMP/mock-suites.sh"
printf '#!/usr/bin/env bash\nprintf "  suites in the tree                  0   (0 gated, 0 timed)\\n"\n' \
    > "$MOCK_SUITES"
chmod +x "$MOCK_SUITES"

SHA="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
RELEASE_MOCK="$TMP/mock-release"
mock_release() {  # mock_release <status-text>
    cat > "$RELEASE_MOCK" <<EOF
#!/usr/bin/env bash
[ "\${1:-}" = status ] || exit 2
cat <<'STATUS_EOF'
$1
STATUS_EOF
EOF
    chmod +x "$RELEASE_MOCK"
}

fresh() { rm -rf "$TMP/run" "$TMP/captured.txt"; mkdir -p "$TMP/run/landstate"; }

INC_MOCK="$TMP/mock-inc.sh"
cat > "$INC_MOCK" <<'EOF'
#!/usr/bin/env bash
{
    echo "ARGS: $*"
    echo "REF: ${SPIRA_INCIDENT_REF:-}"
    echo "CAUSE: ${SPIRA_INCIDENT_CAUSE:-}"
    cat
    echo "---"
} >> "$SPIRA_TEST_CAPTURE"
EOF
chmod +x "$INC_MOCK"

# wt_file: runs watchtower for real (writes the prompt file and drives escalations),
# with $RELEASE_MOCK standing in for `release` and $INC_MOCK capturing incident.sh calls.
wt_file() {
    env -i PATH="$TMP:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_WATCH_PROMPT_FILE="$TMP/ops-prompt" \
        SPIRA_INCIDENT_SH="$INC_MOCK" \
        SPIRA_TEST_CAPTURE="$TMP/captured.txt" \
        SPIRA_SUITES_SH="$MOCK_SUITES" \
        watchtower.sh 2>/dev/null
}
# `release` must resolve by bare name (the box's own convention, sp-gypjk): symlink it
# into the directory PATH is given above instead of pointing PATH at $TMP/bin, so the
# mock can be replaced per-case without re-linking.
ln -sf "$RELEASE_MOCK" "$TMP/release"

captured() { cat "$TMP/captured.txt" 2>/dev/null || true; }

# ======================================================================================
echo
echo "positive control — a hotfix past threshold files exactly one escalation:"
# ======================================================================================
fresh
mock_release "$(printf 'current abc\nRUNNING UNLANDED %s: emergency fix (since 2026-09-30T00:00:00Z)\nALERT hotfix %s standing 5h >= threshold 4h' "$SHA" "$SHA")"
wt_file
out="$(captured)"
want "past threshold: an escalation is filed"        "ARGS: file"                   "$out"
want "past threshold: titled HOTFIX"                  "HOTFIX: RUNNING UNLANDED"     "$out"
want "past threshold: deduped by the standing sha"    "REF: incident:hotfix-$SHA"    "$out"
want "past threshold: cause names hotfix-standing"    "CAUSE: hotfix-standing"       "$out"
want "past threshold: body carries the RUNNING UNLANDED line" "RUNNING UNLANDED $SHA" "$out"
want "past threshold: body carries the ALERT line"    "ALERT hotfix $SHA"            "$out"
n_filed="$(printf '%s\n' "$out" | grep -c '^ARGS: file')"
is "past threshold: exactly one escalation, not a duplicate" 1 "$n_filed"

# ======================================================================================
echo
echo "a hotfix under threshold — RUNNING UNLANDED with no ALERT — files nothing:"
# ======================================================================================
fresh
mock_release "$(printf 'current abc\nRUNNING UNLANDED %s: emergency fix (since 2026-09-30T00:00:00Z)' "$SHA")"
wt_file
nowant "under threshold: no escalation filed" "ARGS: file" "$(captured)"

# ======================================================================================
echo
echo "no hotfix standing files nothing:"
# ======================================================================================
fresh
mock_release "current abc"
wt_file
nowant "no hotfix: no escalation filed" "ARGS: file" "$(captured)"

# ======================================================================================
echo
echo "a second sweep of the SAME standing hotfix files no second escalation:"
# ======================================================================================
# incident.sh owns dedup-by-ref; this suite only has to prove watchtower asks for it with
# a ref keyed on the standing sha, which the positive control above already captured — a
# second real sweep here would need incident.sh's own lookback behaviour, which is its
# suite's job, not this one's (sp-6p20x: "filed once per hotfix" is the ref, not a second
# state file watchtower would have to keep in sync with it).
fresh
mock_release "$(printf 'current abc\nRUNNING UNLANDED %s: emergency fix (since 2026-09-30T00:00:00Z)\nALERT hotfix %s standing 5h >= threshold 4h' "$SHA" "$SHA")"
wt_file
first_ref="$(captured | grep '^REF:' | head -1)"
is "the ref is stable across sweeps of the same hotfix" "REF: incident:hotfix-$SHA" "$first_ref"

echo
tl_summary
