#!/usr/bin/env bash
#
# test-suites-cluster.sh — same-cause reds file one bead, not one per suite.
#
#   ./test-suites-cluster.sh
#
# THE PROPERTY UNDER TEST. When multiple suites in a timed pass fail on the same
# normalised first FAIL line, suites.sh must cluster them and file ONE bead naming
# every member suite — not one bead per suite. Suites that fail DIFFERENTLY must
# each receive their own bead.
#
# THE EVIDENCE THAT MOTIVATED THIS. On 2026-09-12, ten of fourteen test-X.sh-is-red
# beads came from three causes. Two of those three were shared-cause events:
#   - five beads from one fixture-level fault (fixed by file_fixture_fault already)
#   - two beads (sp-mwf, sp-w54) from one gate-spira.sh CWD issue
# Each shared-cause group should have filed one bead. Instead it filed one per suite.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control):
#   - Three suites with the SAME first FAIL line must produce one cluster bead.
#   - Three suites with DISTINCT FAIL lines must each produce their own bead.
#   - Total beads: 4 (not 6).
#
# SUPPRESSION COUNT (law-dedup-must-be-measured):
#   - The pass output reports how many beads the clustering suppressed.
#
# RECURRENCE STABILITY: the cause-cluster ref is stable across passes so repeated
# occurrences bump recurrence, not a fresh bead.
#
# defect: sp-fjb
# covers: spira/suites.sh spira/incident.sh
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-suites-cluster.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-suites-cluster
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up suites-cluster || { echo "test-suites-cluster: could not build a fixture database"; exit 1; }

SH="$TMP/spira"; RUN="$TMP/run"; STATE="$TMP/state"; GATEF="$TMP/gate-suites"
mkdir -p "$SH" "$RUN" "$STATE" "$TMP/repo"
cp "$HERE/suites.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/incident.sh" "$HERE/incident-dedup-decision.py" "$HERE/suite-covers.sh" "$HERE/suite-state.sh" "$SH/"

printf '# the gated sentinel\nspira/test-cl-gated.sh\n' > "$GATEF"

BUDGET=120; PERSUITE=20; STALE=3600; PRIO=3; REPONAME=cluster-repo

# THE bd WRAPPER. See test-suites.sh for the explanation.
TOOLPATH="$TMP/bin"; mkdir -p "$TOOLPATH"
printf '#!/usr/bin/env bash\nHOME=%s exec %s "$@"\n' "$HOME" "$(type -P bd)" > "$TOOLPATH/bd"
chmod +x "$TOOLPATH/bd"

# sut <subcommand> — run suites.sh in a clean environment.
# SPIRA_SUITES_RUNNER_VARS="" and SPIRA_SUITES_SKIP_TESTDB=1 match test-suites.sh's
# rationale: no runner-var wrapping overhead and no shared-fixture build per sut call.
sut() {
    local cmd="$1"; shift
    # SPIRA_SUITES_INLINE=1 — run the planted suites HERE, not in a container.
    # suites.sh now delegates its pass to testenv-batch.sh (law-tests-run-only-through-
    # testenv-batch). This suite is testing suites.sh's OWN logic against fake suites it
    # planted in a scratch tree, so a container would have to be started per invocation to
    # run code that exists only to be counted. Containment is not being waived: this suite
    # is itself run inside a container by the timed pass, so the planted suites are already
    # contained by it.
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_HOME="$SH" SPIRA_REPO="$TMP/repo" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$RUN" \
        SPIRA_SUITES_STATE="$STATE" SPIRA_GATE_SUITES="$GATEF" \
        SPIRA_SUITES_BUDGET="$BUDGET" SPIRA_SUITE_TIMEOUT="$PERSUITE" \
        SPIRA_SUITES_STALE="$STALE" SPIRA_SUITES_PRIORITY="$PRIO" \
        SPIRA_PATH="$TOOLPATH" SPIRA_SUITES_RUNNER_VARS="" SPIRA_INCIDENT_LOCK_WAIT="60" \
        SPIRA_SUITES_SKIP_TESTDB=1 \
        SPIRA_SUITES_INLINE=1 \
        "$@" bash "$SH/suites.sh" "$cmd" 2>&1
}
plant() { cat > "$SH/$1"; chmod +x "$SH/$1"; }
mkdir -p "$TMP/home"

B() { bd -C "$SPIRA_DB" "$@"; }
beads_matching() {
    B list --status open,in_progress --limit 0 --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
key = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if key in (i.get("title") or ""): print(i["id"])
' "$1"
}
count() { printf '%s\n' "$1" | grep -c . || true; }
bead_body() {
    B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
d = d[0] if isinstance(d, list) else d
print(d.get("description",""))
' 2>/dev/null || true
}
all_open_count() {
    B list --status open,in_progress --limit 0 --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
print(len(d if isinstance(d, list) else [d]))
' 2>/dev/null || echo 0
}
clear_results() { find "$STATE" -maxdepth 1 -name '*.result' -delete 2>/dev/null; true; }

plant test-cl-gated.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
exit 0
S

# ======================================================================================
echo
echo "positive control — three suites with the SAME first FAIL line:"
echo "  expect one cluster bead naming all three, not three separate beads"
# ======================================================================================
# THE SAME FAIL MESSAGE. All three suites print the same first FAIL line — a shared
# assertion that the same code path is broken. Clustering must detect this and file one
# bead. The assertion content is deliberately simple; the normalisation's job is to
# strip varying tokens (paths, numbers), not to deduplicate identical strings.
plant test-cl-same-a.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
printf '  FAIL  shared-assertion: wanted [ok] got [broken]\n'
exit 1
S
plant test-cl-same-b.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
printf '  FAIL  shared-assertion: wanted [ok] got [broken]\n'
exit 1
S
plant test-cl-same-c.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
printf '  FAIL  shared-assertion: wanted [ok] got [broken]\n'
exit 1
S

# Three suites with DISTINCT FAIL lines — must each get their own bead.
plant test-cl-diff-p.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
printf '  FAIL  unique-assertion-p: wanted [x] got [y]\n'
exit 1
S
plant test-cl-diff-q.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
printf '  FAIL  unique-assertion-q: wanted [m] got [n]\n'
exit 1
S
plant test-cl-diff-r.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
printf '  FAIL  unique-assertion-r: wanted [j] got [k]\n'
exit 1
S

out="$(sut run)"; run_rc=$?

# The pass must exit 2 (incidents were filed, not a critical error).
is "a pass with reds exits 2" "2" "$run_rc"

# ONE CLUSTER BEAD FOR THE THREE SAME-CAUSE SUITES.
# The cluster bead's title contains "same cause" per file_cluster's title template.
cluster_ids="$(beads_matching 'same cause')"
is "exactly one cluster bead for the three same-cause suites" "1" "$(count "$cluster_ids")"
cluster_id="$(printf '%s\n' "$cluster_ids" | head -1)"
if [ -n "$cluster_id" ]; then
    body="$(bead_body "$cluster_id")"
    want "the cluster bead names suite A" "test-cl-same-a.sh" "$body"
    want "the cluster bead names suite B" "test-cl-same-b.sh" "$body"
    want "the cluster bead names suite C" "test-cl-same-c.sh" "$body"
fi

# NO PER-SUITE BEADS FOR THE CLUSTERED SUITES. The cluster bead title does not contain
# the individual suite name, so a match here means a spurious individual bead was filed.
is "no individual bead for same-a" "0" "$(count "$(beads_matching 'test-cl-same-a.sh')")"
is "no individual bead for same-b" "0" "$(count "$(beads_matching 'test-cl-same-b.sh')")"
is "no individual bead for same-c" "0" "$(count "$(beads_matching 'test-cl-same-c.sh')")"

# THREE SEPARATE BEADS FOR THE DISTINCT-CAUSE SUITES.
is "separate bead for diff-p" "1" "$(count "$(beads_matching 'test-cl-diff-p.sh')")"
is "separate bead for diff-q" "1" "$(count "$(beads_matching 'test-cl-diff-q.sh')")"
is "separate bead for diff-r" "1" "$(count "$(beads_matching 'test-cl-diff-r.sh')")"

# TOTAL: 4 beads (1 cluster + 3 distinct), not 6.
is "total open beads: 4, not 6" "4" "$(all_open_count)"

# THE SUPPRESSION COUNT IS IN THE PASS OUTPUT (law-dedup-must-be-measured).
# The cluster of 3 suppresses 2 beads; the 3 distinct add 0; total suppressed = 2.
want "the pass reports how many beads clustering suppressed" "suppressed by clustering" "$out"
want "suppression count of 2" "2 suppressed" "$out"

# ======================================================================================
echo
echo "negative control — distinct FAIL lines do not cluster:"
echo "  already verified above (three diff-* suites → three beads, not one)"
# ======================================================================================
# The three distinct-cause suites above already prove this. No further setup needed.

# ======================================================================================
echo
echo "recurrence stability — same shared cause next pass bumps the same bead, not a new one:"
# ======================================================================================
# The cause-cluster:<cfp> ref is stable across passes: the same normalised FAIL line
# produces the same cfp every time. incident.sh deduplicates on the external_ref, so
# a second pass with the same shared FAIL line must find the existing cluster bead and
# record a recurrence note, rather than filing a fresh bead. The positive control is
# that the second pass returns the SAME bead ID that the first pass created.
sut run >/dev/null
is "second pass with same shared cause: still one cluster bead, not two" "1" \
   "$(count "$(beads_matching 'same cause')")"
cluster_id2="$(beads_matching 'same cause' | head -1)"
# The bead ID must be the same as the one found after the first pass.
is "second pass reused the same cluster bead, not a new one" "$cluster_id" "$cluster_id2"

# ======================================================================================
echo
echo "a singleton red (unshared cause) files via the normal path:"
# ======================================================================================
plant test-cl-solo.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
printf '  FAIL  solo assertion that nobody shares\n'
exit 1
S
sut run >/dev/null
is "a singleton red gets its own bead" "1" "$(count "$(beads_matching 'test-cl-solo.sh')")"

echo
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
