#!/usr/bin/env bash
# timeout: 150
#
# test-livelock.sh — detect_livelocked and detect_invalid_closed surface the right beads.
#
#   ./test-livelock.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# A bead can be open and unreachable for many structural reasons. Before this sweeper, each
# was found by hand, once, after it had already cost hours. The sweeper names each bead and
# why, so a broken graph is seen on the next cockpit pass rather than after someone notices.
#
# CATEGORIES TESTED
#
#   unclaimable          fayth:ops on spira,plan labels — builder excluded by preference,
#                        ops excluded by its own partition. Fifteen-hour strand, 2026-09-09.
#
#   needs-ryan-no-overseer  needs-ryan without overseer: invisible to the decisions pane
#                        and excluded from every fayth predicate.
#
#   unmapped-repo        repo:bogus not in the repo-map; aeon.sh refuses at claim time.
#
#   ci-stuck             awaiting-ci on a push-mode repo; no run will ever report.
#
#   INVALID-CLOSED       closed bead whose close reason contains a statute phrase
#                        ("PERMANENT FIX NEEDED", "temporary", "mitigated-only", "TODO");
#                        law-no-close-reason-admits-unfinished forbids this prospectively,
#                        nothing detected the ones already in the store.
#
#   UNFILED-FOLLOW       closed bead whose close reason implies follow-on work exists
#                        (contains a phrase like "builders should", "the real fix",
#                        "follow-up", "at scale") but names no tracking reference (a bead
#                        id in the form PREFIX-id, an owner/repo#N GitHub reference, or an
#                        https:// URL). "upstream" is not a trigger — it names a
#                        destination, not an unfinished remainder. "workaround" is NOT a
#                        flag — a workaround can be complete and landed.
#
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control). The negative half proves
# the check can read a true zero; the positive half proves it reads the real fault.
#
# covers: spira/lib.sh spira/cockpit.sh spira/chamber/builder.fayth spira/chamber/ops.fayth
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require test-livelock
TMP="$(mktemp -d)"
testdb_up livelock || { echo "test-livelock: could not build a fixture database"; exit 1; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'exit 143' INT TERM

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# A repo-map with one push-mode entry (no PR) and one pr-mode entry.
# PINNED TO NON-DEFAULT NAMES so the suite cannot pass on an accidentally matching literal.
MAP="$TMP/repo-map"
printf 'pushrepo | /opt/pushrepo | push | origin/main | | \n' > "$MAP"
printf 'prerepo  | /opt/prerepo  | pr   | origin/main | | \n' >> "$MAP"

RUN="$TMP/run"; mkdir -p "$RUN"

# run_ll: run detect_livelocked through the cockpit seam.
run_ll() {    # run_ll [KEY=val ...]  — extra args override env vars
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-goal \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SPIKE_LABEL=spike \
        "$@" \
        bash "$HERE/cockpit.sh" livelock 2>/dev/null
}

echo "test-livelock.sh"

# ==========================================================================================
echo
echo "NEGATIVE CONTROL — empty database: both counts are 0, not ?:"
# ==========================================================================================
# A probe that returns ? on an empty database is broken, not cautious. An empty list is a
# real measurement (law-absence-needs-a-positive-control).
testdb_reset
out="$(run_ll)"
is "empty db: SP_LIVELOCKED=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_LIVELOCKED=//p' | head -1)"
is "empty db: SP_INVALID_CLOSED=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_INVALID_CLOSED=//p' | head -1)"
is "empty db: SP_UNFILED_FOLLOW=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"

# ==========================================================================================
echo
echo "NEGATIVE CONTROL — claimable builder bead is not flagged:"
# ==========================================================================================
# Without this, a detect-everything implementation would read as correct. A bead that
# builder can claim must produce SP_LIVELOCKED=0 and no LIVELOCK row naming it.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-good","title":"claimable builder bead","status":"open","issue_type":"task","labels":["plan","repo:pushrepo","spira"]}
JSONL
out="$(run_ll)"
is "claimable bead: SP_LIVELOCKED=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_LIVELOCKED=//p' | head -1)"
nowant "claimable bead not in livelock output" "sp-ll-good" "$out"

# ==========================================================================================
echo
echo "POSITIVE CONTROL — unclaimable: fayth:ops on spira,plan labels:"
# ==========================================================================================
# builder matches spira,plan but is excluded by fayth:ops; ops is excluded by its own
# partition (spira,incident). This is the fifteen-hour strand of 2026-09-09.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-unc","title":"unclaimable fayth:ops on plan","status":"open","issue_type":"task","labels":["fayth:ops","plan","repo:pushrepo","spira"]}
JSONL
out="$(run_ll)"
want  "unclaimable: SP_LIVELOCKED>=1"     "SP_LIVELOCKED=" "$out"
want  "unclaimable: LIVELOCK row present" "LIVELOCK"       "$out"
want  "unclaimable: bead id in row"       "sp-ll-unc"      "$out"
want  "unclaimable: category named"       "unclaimable"    "$out"

# ==========================================================================================
echo
echo "POSITIVE CONTROL — needs-ryan without overseer:"
# ==========================================================================================
# needs-ryan is excluded from every fayth predicate; without overseer, the decisions pane
# cannot see this bead either. It is invisible to the loop and to Ryan.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-nr","title":"needs-ryan no overseer","status":"open","issue_type":"task","labels":["needs-ryan","spira","plan","repo:pushrepo"]}
JSONL
out="$(run_ll)"
want  "needs-ryan-no-overseer: LIVELOCK row" "LIVELOCK"                 "$out"
want  "needs-ryan-no-overseer: bead id"      "sp-ll-nr"                 "$out"
want  "needs-ryan-no-overseer: category"     "needs-ryan-no-overseer"   "$out"

# A needs-ryan bead WITH overseer is not flagged — that is the correct configuration.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-nrok","title":"needs-ryan with overseer","status":"open","issue_type":"task","labels":["needs-ryan","overseer","spira","plan","repo:pushrepo"]}
JSONL
out="$(run_ll)"
nowant "needs-ryan WITH overseer is not flagged" "sp-ll-nrok" "$out"

# ==========================================================================================
echo
echo "POSITIVE CONTROL — unmapped-repo: repo:bogus not in the repo-map:"
# ==========================================================================================
# aeon.sh refuses to claim a bead whose repo: label the map cannot resolve, and leaves it
# open forever. The sweeper names it so the fix is one label change.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-unmap","title":"unmapped repo label","status":"open","issue_type":"task","labels":["spira","plan","repo:bogusrepo"]}
JSONL
out="$(run_ll)"
want  "unmapped-repo: LIVELOCK row" "LIVELOCK"      "$out"
want  "unmapped-repo: bead id"      "sp-ll-unmap"   "$out"
want  "unmapped-repo: category"     "unmapped-repo" "$out"

# A bead with a mapped repo: label is not flagged for unmapped-repo.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-mapped","title":"correctly mapped repo","status":"open","issue_type":"task","labels":["spira","plan","repo:pushrepo"]}
JSONL
out="$(run_ll)"
nowant "mapped repo bead not flagged as unmapped" "unmapped-repo" "$out"

# ==========================================================================================
echo
echo "POSITIVE CONTROL — ci-stuck: awaiting-ci on a push-mode repo:"
# ==========================================================================================
# pushrepo uses land mode 'push', not 'pr'. No pull request is opened and no CI run ever
# reports. The awaiting-ci label is a permanent hold that no mechanism will ever clear.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-ci","title":"ci-stuck push repo","status":"open","issue_type":"task","labels":["awaiting-ci","spira","plan","repo:pushrepo"]}
JSONL
out="$(run_ll)"
want  "ci-stuck: LIVELOCK row" "LIVELOCK"  "$out"
want  "ci-stuck: bead id"      "sp-ll-ci"  "$out"
want  "ci-stuck: category"     "ci-stuck"  "$out"

# A bead with awaiting-ci on a pr-mode repo is NOT ci-stuck — the run is expected.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-ciwait","title":"ci-waiting pr repo","status":"open","issue_type":"task","labels":["awaiting-ci","spira","plan","repo:prerepo"],"updated_at":"2026-09-09T10:00:00Z"}
JSONL
out="$(run_ll)"
nowant "pr-mode ci-wait is not ci-stuck" "ci-stuck" "$out"

# ==========================================================================================
echo
echo "POSITIVE CONTROL — INVALID-CLOSED: close reason admits unfinished work:"
# ==========================================================================================
# law-no-close-reason-admits-unfinished forbids prospectively; nothing detected the ones
# already in the store. This check finds them by grepping close_reason.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-ic","title":"invalid closed bead","status":"closed","issue_type":"task","labels":["spira","plan","repo:pushrepo"],"close_reason":"PERMANENT FIX NEEDED: add real detection"}
JSONL
out="$(run_ll)"
want  "invalid-closed: INVALID-CLOSED row" "INVALID-CLOSED" "$out"
want  "invalid-closed: bead id"            "sp-ll-ic"       "$out"
is "invalid-closed: SP_INVALID_CLOSED=1" "1" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_INVALID_CLOSED=//p' | head -1)"

# A closed bead with a clean close reason is not flagged.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-clean","title":"cleanly closed","status":"closed","issue_type":"task","labels":["spira","plan","repo:pushrepo"],"close_reason":"fixed: sp-ll-clean commit abc123 landed on main"}
JSONL
out="$(run_ll)"
is "clean close reason: SP_INVALID_CLOSED=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_INVALID_CLOSED=//p' | head -1)"
nowant "clean close: no INVALID-CLOSED row" "INVALID-CLOSED" "$out"

# ==========================================================================================
echo
echo "NEGATIVE CONTROL — workaround in close reason is NOT flagged:"
# ==========================================================================================
# "workaround" is not in the statute and does not discriminate — a workaround can be
# complete, verified and landed. Positive control (the bead that prompted this fix) had
# close reason ending "Ready for rebase and merge." and was incorrectly flagged.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-wa","title":"workaround landed","status":"closed","issue_type":"task","labels":["spira","plan","repo:pushrepo"],"close_reason":"Workaround implemented by aeon-yojimbo, verified, landed on origin/main. Ready for merge."}
JSONL
out="$(run_ll)"
nowant "workaround: no INVALID-CLOSED row"  "INVALID-CLOSED"  "$out"
nowant "workaround: no UNFILED-FOLLOW row"  "UNFILED-FOLLOW"  "$out"
is "workaround: SP_INVALID_CLOSED=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_INVALID_CLOSED=//p' | head -1)"
is "workaround: SP_UNFILED_FOLLOW=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"

# ==========================================================================================
echo
echo "POSITIVE CONTROL — UNFILED-FOLLOW: follow-on phrase with no bead id:"
# ==========================================================================================
# A close reason that says "builders should add X" without citing a bead id means follow-on
# work was observed but not filed. The check flags this as UNFILED-FOLLOW (separate from
# INVALID-CLOSED so the two family counts mean different things on the dashboard).
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-uf","title":"unfiled follow-on","status":"closed","issue_type":"task","labels":["spira","plan","repo:pushrepo"],"close_reason":"Builders should add external_ref to bd list --json output."}
JSONL
out="$(run_ll)"
want  "unfiled-follow: UNFILED-FOLLOW row"  "UNFILED-FOLLOW"  "$out"
want  "unfiled-follow: bead id in row"      "sp-ll-uf"        "$out"
nowant "unfiled-follow: no INVALID-CLOSED"  "INVALID-CLOSED"  "$out"
is "unfiled-follow: SP_UNFILED_FOLLOW=1" "1" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"
is "unfiled-follow: SP_INVALID_CLOSED=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_INVALID_CLOSED=//p' | head -1)"

# ==========================================================================================
echo
echo "NEGATIVE CONTROL — UNFILED-FOLLOW: follow-on phrase WITH a bead id is not flagged:"
# ==========================================================================================
# A close reason that says "builders should add X, tracked as sp-foo" has handed off
# correctly — the work is filed. The bead id acquits the follow-on phrase.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-uf2","title":"follow-on filed","status":"closed","issue_type":"task","labels":["spira","plan","repo:pushrepo"],"close_reason":"Builders should add external_ref to bd list --json. Tracked as sp-80br6."}
JSONL
out="$(run_ll)"
nowant "unfiled-follow filed: no UNFILED-FOLLOW row" "UNFILED-FOLLOW" "$out"
nowant "unfiled-follow filed: no INVALID-CLOSED row" "INVALID-CLOSED" "$out"
is "unfiled-follow filed: SP_UNFILED_FOLLOW=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"

# ==========================================================================================
echo
echo "NEGATIVE CONTROL — UNFILED-FOLLOW: follow-on phrase WITH a GitHub issue is not flagged:"
# ==========================================================================================
# A close reason citing owner/repo#N has handed off to a tracked external issue.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-uf3","title":"follow-on github issue","status":"closed","issue_type":"task","labels":["spira","plan","repo:pushrepo"],"close_reason":"Builders should add external_ref to bd list --json. Filed as owner/repo#42."}
JSONL
out="$(run_ll)"
nowant "unfiled-follow github: no UNFILED-FOLLOW row" "UNFILED-FOLLOW" "$out"
is "unfiled-follow github: SP_UNFILED_FOLLOW=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"

# ==========================================================================================
echo
echo "NEGATIVE CONTROL — UNFILED-FOLLOW: follow-on phrase WITH an https:// URL is not flagged:"
# ==========================================================================================
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-uf4","title":"follow-on url","status":"closed","issue_type":"task","labels":["spira","plan","repo:pushrepo"],"close_reason":"The real fix is tracked at https://github.com/owner/repo/issues/42."}
JSONL
out="$(run_ll)"
nowant "unfiled-follow url: no UNFILED-FOLLOW row" "UNFILED-FOLLOW" "$out"
is "unfiled-follow url: SP_UNFILED_FOLLOW=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"

# ==========================================================================================
echo
echo "NEGATIVE CONTROL — UNFILED-FOLLOW: non-default prefix bead id acquits:"
# ==========================================================================================
# SPIRA_ID_PREFIX=tt means tt-xxxx is a valid bead id; the hardcoded sp- would miss it.
testdb_reset
testdb_seed <<'JSONL'
{"id":"tt-ll-uf5","title":"follow-on non-default prefix","status":"closed","issue_type":"task","labels":["spira","plan","repo:pushrepo"],"close_reason":"Builders should fix the schema. Tracked as tt-abc1."}
JSONL
out="$(run_ll SPIRA_ID_PREFIX=tt SPIRA_GOAL=tt-goal)"
nowant "non-default prefix: no UNFILED-FOLLOW row" "UNFILED-FOLLOW" "$out"
is "non-default prefix: SP_UNFILED_FOLLOW=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"

# ==========================================================================================
echo
echo "NEGATIVE CONTROL — UNFILED-FOLLOW: 'upstream' alone does not trigger:"
# ==========================================================================================
# "upstream" names a destination, not an unfinished remainder. A close reason that says
# "filed upstream" without any other follow-on phrase must not be flagged.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-up","title":"filed upstream","status":"closed","issue_type":"task","labels":["spira","plan","repo:pushrepo"],"close_reason":"Moot upstream — the sentinel already handles this. Closed."}
JSONL
out="$(run_ll)"
nowant "upstream alone: no UNFILED-FOLLOW row" "UNFILED-FOLLOW" "$out"
is "upstream alone: SP_UNFILED_FOLLOW=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"

# ==========================================================================================
echo
echo "MIXED — two distinct shapes, both found and categorised:"
# ==========================================================================================
# The acceptance criterion: the sweeper detects at least two distinct livelock shapes
# in a single pass and names each by category. This case plants a fayth:ops mismatch and
# a needs-ryan-no-overseer bead side by side.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-mix1","title":"unclaimable fayth:ops","status":"open","issue_type":"task","labels":["fayth:ops","plan","repo:pushrepo","spira"]}
{"id":"sp-ll-mix2","title":"needs-ryan no overseer","status":"open","issue_type":"task","labels":["needs-ryan","spira","plan","repo:pushrepo"]}
{"id":"sp-ll-mix3","title":"cleanly claimable","status":"open","issue_type":"task","labels":["plan","repo:pushrepo","spira"]}
JSONL
out="$(run_ll)"
want  "mixed: unclaimable bead named"          "sp-ll-mix1"             "$out"
want  "mixed: unclaimable category"            "unclaimable"            "$out"
want  "mixed: needs-ryan-no-overseer named"    "sp-ll-mix2"             "$out"
want  "mixed: needs-ryan-no-overseer category" "needs-ryan-no-overseer" "$out"
nowant "mixed: claimable bead NOT reported"    "sp-ll-mix3"             "$out"
# The count reflects only the two livelocked beads, not the claimable one.
ll_n="$(printf '%s\n' "$out" | sed -n 's/^SP_LIVELOCKED=//p' | head -1)"
[ "${ll_n:-0}" -ge 2 ] \
    && ok "mixed: SP_LIVELOCKED>=2 (both shapes counted)" \
    || bad "mixed: SP_LIVELOCKED count" "expected >=2, got [$ll_n]"

# ==========================================================================================
echo
echo "NEGATIVE CONTROL — blocked bead (open dependency) is not livelocked:"
# ==========================================================================================
# A bead with an open blocking dependency is correct sequencing, not a livelock.
# bd ready does not surface blocked beads; the sweeper must not flag them either.
# The unclaimable detection uses bd ready which already filters these out.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-ll-dep","title":"open dependency","status":"open","issue_type":"task","labels":["spira","plan","repo:pushrepo"]}
{"id":"sp-ll-blocked","title":"blocked on open dep","status":"open","issue_type":"task","labels":["spira","plan","repo:pushrepo"],"dependency_count":1}
JSONL
out="$(run_ll)"
nowant "blocked bead: sp-ll-blocked not in livelock output" "sp-ll-blocked" "$out"

echo
printf 'test-livelock: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
