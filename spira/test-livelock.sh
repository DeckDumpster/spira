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
#   ask-no-overseer      SPIRA_ASK_LABEL without overseer: invisible to the decisions pane
#                        and excluded from every fayth predicate.
#
#   unmapped-repo        repo:bogus not in the repo-map; aeon.sh refuses at claim time.
#
#   ci-stuck             awaiting-ci on a push-mode repo; no run will ever report.
#
#   INVALID-CLOSED / UNFILED-FOLLOW   close_reason classification, covered exhaustively
#                        (every phrase, every tracking-reference exemption) by
#                        test-close-reason-flags.sh against close-reason-flags.py directly,
#                        no database. This suite keeps ONE row of each proving
#                        detect_invalid_closed's output actually reaches cockpit-collect
#                        livelock's report — the integration, not the classification.
#
# EVERY CATEGORY IS A PAIR (law-absence-needs-a-positive-control): a true positive and a
# true negative coexist in ONE seed and ONE `cockpit-collect probe livelock` run, so a detector that
# flagged everything (or nothing) could not pass by accident. Reusing one seed across every
# category — rather than resetting the database once per case — is the same property proven
# with the database built once.
#
# tier: T2
# covers: spira/lib.sh cockpit-collect/src/* spira/close-reason-flags.py spira/chamber/builder.fayth spira/chamber/ops.fayth
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"
testdb_require test-livelock
TMP="$(mktemp -d)"
testdb_up livelock || { echo "test-livelock: could not build a fixture database"; exit 1; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'exit 143' INT TERM


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
        SPIRA_REPO_MAP="$MAP" \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SPIKE_LABEL=spike \
        SPIRA_SCOPE_LABEL="${SPIRA_SCOPE_LABEL:-}" \
        "$@" \
        cockpit-collect probe livelock 2>/dev/null
}
field() { printf '%s\n' "$1" | sed -n "s/^$2=//p" | head -1; }

echo "test-livelock.sh"

# ==========================================================================================
echo
echo "NEGATIVE CONTROL — empty database: all counts are 0, not ?:"
# ==========================================================================================
# A probe that returns ? on an empty database is broken, not cautious. An empty list is a
# real measurement (law-absence-needs-a-positive-control).
testdb_reset
out="$(run_ll)"
is "empty db: SP_LIVELOCKED=0"     "0" "$(field "$out" SP_LIVELOCKED)"
is "empty db: SP_INVALID_CLOSED=0" "0" "$(field "$out" SP_INVALID_CLOSED)"
is "empty db: SP_UNFILED_FOLLOW=0" "0" "$(field "$out" SP_UNFILED_FOLLOW)"

# ==========================================================================================
echo
echo "ONE SEED, every category's positive and negative control side by side:"
# ==========================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-ll-good","title":"claimable builder bead","status":"open","issue_type":"task","labels":["plan","repo:pushrepo","${SPIRA_SCOPE_LABEL}"]}
{"id":"sp-ll-unc","title":"unclaimable fayth:ops on plan","status":"open","issue_type":"task","labels":["fayth:ops","plan","repo:pushrepo","${SPIRA_SCOPE_LABEL}"]}
{"id":"sp-ll-nr","title":"needs-ryan no overseer","status":"open","issue_type":"task","labels":["needs-ryan","${SPIRA_SCOPE_LABEL}","plan","repo:pushrepo"]}
{"id":"sp-ll-nrok","title":"needs-ryan with overseer","status":"open","issue_type":"task","labels":["needs-ryan","overseer","${SPIRA_SCOPE_LABEL}","plan","repo:pushrepo"]}
{"id":"sp-ll-unmap","title":"unmapped repo label","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:bogusrepo"]}
{"id":"sp-ll-mapped","title":"correctly mapped repo","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:pushrepo"]}
{"id":"sp-ll-ci","title":"ci-stuck push repo","status":"open","issue_type":"task","labels":["awaiting-ci","${SPIRA_SCOPE_LABEL}","plan","repo:pushrepo"]}
{"id":"sp-ll-ciwait","title":"ci-waiting pr repo","status":"open","issue_type":"task","labels":["awaiting-ci","${SPIRA_SCOPE_LABEL}","plan","repo:prerepo"],"updated_at":"2026-09-09T10:00:00Z"}
{"id":"sp-ll-ic","title":"invalid closed bead","status":"closed","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:pushrepo"],"close_reason":"PERMANENT FIX NEEDED: add real detection"}
{"id":"sp-ll-uf","title":"unfiled follow-on","status":"closed","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:pushrepo"],"close_reason":"Builders should add external_ref to bd list --json output."}
{"id":"sp-ll-clean","title":"cleanly closed","status":"closed","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:pushrepo"],"close_reason":"fixed: sp-ll-clean commit abc123 landed on main"}
{"id":"sp-ll-dep","title":"open dependency","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:pushrepo"]}
{"id":"sp-ll-blocked","title":"blocked on open dep","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:pushrepo"],"dependency_count":1}
JSONL
out="$(run_ll)"

echo "  — claimable bead is never flagged:"
nowant "claimable bead not in livelock output" "sp-ll-good" "$out"

echo "  — unclaimable: fayth:ops on spira,plan labels:"
# builder matches spira,plan but is excluded by fayth:ops; ops is excluded by its own
# partition (spira,incident). This is the fifteen-hour strand of 2026-09-09.
want "unclaimable: LIVELOCK row present" "LIVELOCK sp-ll-unc" "$out"
want "unclaimable: category named"      "unclaimable"        "$out"

echo "  — ask-no-overseer: needs-ryan without overseer is flagged, with overseer is not:"
want   "ask-no-overseer: LIVELOCK row"           "LIVELOCK sp-ll-nr" "$out"
want   "ask-no-overseer: category"               "ask-no-overseer"   "$out"
nowant "needs-ryan WITH overseer is not flagged" "sp-ll-nrok"        "$out"

echo "  — unmapped-repo: repo:bogus not in the repo-map, a mapped repo is not flagged:"
want   "unmapped-repo: LIVELOCK row"             "LIVELOCK sp-ll-unmap" "$out"
want   "unmapped-repo: category"                 "unmapped-repo"        "$out"
nowant "mapped repo bead not flagged as unmapped" "sp-ll-mapped"        "$out"

echo "  — ci-stuck: awaiting-ci on a push-mode repo is stuck; on a pr-mode repo it is not:"
want   "ci-stuck: LIVELOCK row"         "LIVELOCK sp-ll-ci" "$out"
want   "ci-stuck: category"             "ci-stuck"           "$out"
nowant "pr-mode ci-wait is not ci-stuck" "sp-ll-ciwait"      "$out"

echo "  — INVALID-CLOSED / UNFILED-FOLLOW reach the report (classification itself: T1):"
want   "invalid-closed row reaches the sweeper"        "INVALID-CLOSED sp-ll-ic" "$out"
want   "unfiled-follow row reaches the sweeper"        "UNFILED-FOLLOW sp-ll-uf" "$out"
nowant "a clean close reason is not flagged either way" "sp-ll-clean"             "$out"

echo "  — blocked bead (open dependency) is not livelocked:"
# bd ready does not surface blocked beads; the sweeper must not flag them either.
nowant "blocked bead: sp-ll-blocked not in livelock output" "sp-ll-blocked" "$out"

echo "  — counts reflect exactly the flagged beads, not the negative controls:"
is "SP_LIVELOCKED counts the four livelock categories"   "4" "$(field "$out" SP_LIVELOCKED)"
is "SP_INVALID_CLOSED counts the one invalid-closed bead" "1" "$(field "$out" SP_INVALID_CLOSED)"
is "SP_UNFILED_FOLLOW counts the one unfiled-follow bead" "1" "$(field "$out" SP_UNFILED_FOLLOW)"

tl_summary
