#!/usr/bin/env bash
#
# test-check2-reclaim.sh — CHECK 2's dead-worker reaper skips IN_PROGRESS beads whose
#   only open dep carries the ask label, and stops skipping once that dep closes.
#
#   ./test-check2-reclaim.sh
#
# WHY THIS EXISTS. sp-mfa4 hit the reclaim loop six times: it was IN_PROGRESS because its
# aeon correctly exited after diagnosing a needs-ryan dep, but CHECK 2's time-based reaper
# cannot distinguish "correctly waiting" from "genuinely dead". It reclaimed the bead,
# summoned a new aeon, which re-derived the same diagnosis and exited — a ~180m-period
# loop. check2_protect_waiting (lib.sh) labels the bead SPIRA_RECLAIM_SKIP_LABEL while its
# only open dep carries the ask label, so the reaper's --exclude-label skips it. The label
# is removed when the dep closes, so the reaper's exclude-label no longer matches it.
#
# FIVE CASES. THE REAPER ITSELF IS NEVER INVOKED — every case asserts on
# check2_protect_waiting's label state, not on a reclaim actually firing:
#   1. POSITIVE CONTROL (dead worker, no deps) — not given the skip label.
#      Without this, a protect-everything implementation reads as correct.
#   2. PROTECTED (only open dep carries ask label) — skip label applied.
#   3. UNPROTECTED AFTER DEP CLOSES — skip label removed.
#   4. NOT PROTECTED (open dep without ask label) — no skip label applied.
#   5. MIXED DEPS (one ask dep, one non-ask open dep) — no skip label applied.
#
# A REAL bd ON A FIXTURE DATABASE (law-prefer-the-real-dependency). check2_protect_waiting
# calls bd label add/remove and bd show; a stub would drift silently and prove nothing
# about the real label path.
#
# defect: sp-rzyl
# covers: spira/sentinel.sh spira/lib.sh
# hermetic-ok: uses a fixture database, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
has()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lacks(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check2-reclaim
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up check2reclaim || { echo "test-check2-reclaim: could not build fixture database"; exit 1; }

# Stub what sentinel.sh defines but lib.sh needs.
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
acted=0; progressed=0
act()      { acted=$((acted+1)); }
progress() { progressed=$((progressed+1)); act "$@"; }
log()      { : ; }   # suppress log noise in test output
# shellcheck disable=SC1090
. "$HERE/lib.sh"

B() { bd -C "$SPIRA_DB" "$@"; }

echo "test-check2-reclaim.sh"

# Helper: get labels for a bead as a space-separated string.
labels_of() { B label list "$1" 2>/dev/null | tr -d ' -\n' | tr ',' ' '; }

# The ask label and skip label used for these tests (fixture configuration).
ASK="${SPIRA_ASK_LABEL:-needs-operator}"
SKIP="${SPIRA_RECLAIM_SKIP_LABEL:-spira-waiting-operator}"

# ======================================================================================
echo
echo "case 1 — positive control: dead worker (no deps) is NOT given the skip label:"
# ======================================================================================
# An aeon died for an unrelated reason; its bead has no deps. check2_protect_waiting must
# NOT mark it as protected — that would block the reaper from doing its job.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-dead1","title":"dead worker","status":"in_progress","issue_type":"task","labels":["plan","repo:spira","spira"],"assignee":"aeon-dead"}
JSONL
acted=0
check2_protect_waiting
lacks "dead worker: skip label not applied" "$SKIP" "$(B label list sp-dead1 2>/dev/null)"
is    "dead worker: no act recorded" "0" "$acted"

# ======================================================================================
echo
echo "case 2 — protected: only open dep carries the ask label → skip label applied:"
# ======================================================================================
# This is the exact state sp-mfa4 was in. The bead is IN_PROGRESS; its dep is the
# needs-ryan question the aeon filed and exited for.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-ask1","title":"a decision","status":"open","issue_type":"decision","labels":["$ASK","plan","spira"],"assignee":""}
{"id":"sp-work1","title":"work blocked on ryan","status":"in_progress","issue_type":"task","labels":["plan","repo:spira","spira"],"assignee":"aeon-x","dependencies":[{"depends_on_id":"sp-ask1","type":"blocks"}]}
JSONL
acted=0
check2_protect_waiting
has   "blocked bead: skip label applied" "$SKIP" "$(B label list sp-work1 2>/dev/null)"
is    "blocked bead: one act recorded" "1" "$acted"

# ======================================================================================
echo
echo "case 3 — unprotected after dep closes: skip label removed, reaper can fire:"
# ======================================================================================
# Ryan answered. The dep closed. check2_protect_waiting must remove the skip label so the
# reaper's --exclude-label no longer matches (the reaper itself is not invoked here — its
# reclaim on the next pass is the caller's property, not this suite's).
testdb_reset
testdb_seed <<JSONL
{"id":"sp-ask2","title":"a decision","status":"closed","issue_type":"decision","labels":["$ASK","plan","spira"]}
{"id":"sp-work2","title":"work whose dep just closed","status":"in_progress","issue_type":"task","labels":["plan","repo:spira","spira","$SKIP"],"assignee":"aeon-x","dependencies":[{"depends_on_id":"sp-ask2","type":"blocks"}]}
JSONL
acted=0
check2_protect_waiting
lacks "dep closed: skip label removed" "$SKIP" "$(B label list sp-work2 2>/dev/null)"
is    "dep closed: one act recorded" "1" "$acted"

# ======================================================================================
echo
echo "case 4 — not protected: open dep without ask label → no skip label:"
# ======================================================================================
# The bead has an open dep that is NOT a needs-ryan bead (a normal work dep). The aeon
# did not correctly-pause on a ryan dep; it may be a genuine dead worker. No protection.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-other1","title":"prerequisite","status":"open","issue_type":"task","labels":["plan","repo:spira","spira"],"assignee":""}
{"id":"sp-work3","title":"work with non-ryan dep","status":"in_progress","issue_type":"task","labels":["plan","repo:spira","spira"],"assignee":"aeon-y","dependencies":[{"depends_on_id":"sp-other1","type":"blocks"}]}
JSONL
acted=0
check2_protect_waiting
lacks "non-ryan dep: skip label not applied" "$SKIP" "$(B label list sp-work3 2>/dev/null)"
is    "non-ryan dep: no act recorded" "0" "$acted"

# ======================================================================================
echo
echo "case 5 — mixed deps: one ask dep + one non-ask open dep → not protected:"
# ======================================================================================
# The bead has BOTH a ryan dep AND a regular dep open. The condition for protection is
# "all open deps carry the ask label". One non-ask dep disqualifies it.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-ask3","title":"ryan decision","status":"open","issue_type":"decision","labels":["$ASK","plan","spira"],"assignee":""}
{"id":"sp-other2","title":"other prereq","status":"open","issue_type":"task","labels":["plan","repo:spira","spira"],"assignee":""}
{"id":"sp-work4","title":"work with mixed deps","status":"in_progress","issue_type":"task","labels":["plan","repo:spira","spira"],"assignee":"aeon-z","dependencies":[{"depends_on_id":"sp-ask3","type":"blocks"},{"depends_on_id":"sp-other2","type":"blocks"}]}
JSONL
acted=0
check2_protect_waiting
lacks "mixed deps: skip label not applied" "$SKIP" "$(B label list sp-work4 2>/dev/null)"
is    "mixed deps: no act recorded" "0" "$acted"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
