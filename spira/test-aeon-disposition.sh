#!/usr/bin/env bash
#
# test-aeon-disposition.sh — open_ask_blocker (lib.sh), the decision_blocked input to the
# teardown decision pulled out of aeon.sh cleanup() (sp-eq8a4.2.1). Pure: no worktree, no
# database, no systemd.
#
# RETIRED (sp-j89pd, wave 4.2): aeon_disposition and bead_has_label (lib.sh) had zero live
# callers — both are now aeon::decide::disposition and aeon::bd::has_label (aeon/src/decide.rs,
# aeon/src/bd.rs), exercised by aeon/src/tests.rs::poison_raced_releases_and_records and by
# aeon_disposition's own Rust table test. Their rows here are deleted with them. UC-aeon-
# execution-11 stays covered by test-aeon-teardown-e2e.sh / test-rapid-recur.sh /
# test-thrash-teardown.sh; UC-aeon-execution-03 loses its last suite and is marked
# [use_case.uncovered] in docs/test-plan/aeon-execution.toml (see §13 there).
#
# defect: sp-dvsqc sp-2a4hd
# tier: T1
# covers: spira/lib.sh aeon/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
# Non-default, explicit runtime root: lib.sh writes here on source (mkdir -p), and this
# must never be the real installed $SPIRA_RUN (law-probe-a-fixture-not-production).
export SPIRA_INSTANCE=disptest SPIRA_RUN="$TMP/run"
. "$HERE/lib.sh"

# ---- open_ask_blocker — the decision_blocked input to the teardown decision ----------
# Rehomed from test-aeon-teardown-e2e.sh (sp-5t53s), cut there for the area's 60s cap.
# Same fixtures the e2e row drove through a live aeon.sh, asserted here as a table instead.

open_ask_blocker '[{"dependencies":[{"status":"open","labels":["needs-operator"],"dependency_type":"blocks","title":"decide"}]}]' sp-x
wantrc "open_ask_blocker: open ask-labelled blocks dep IS a blocker (positive control)" 0 $?

# sp-dvsqc: mail.sh wires a non-decision cited bead's ask via `dep relate`, which is
# dependency_type "relates-to" — must NOT be read as a blocker (SEEN RED before the fix).
open_ask_blocker '[{"dependencies":[{"status":"open","labels":["needs-operator"],"dependency_type":"relates-to","title":"decide"}]}]' sp-x
wantrc "open_ask_blocker: relates-to edge is NOT a blocker (sp-dvsqc)" 1 $?

open_ask_blocker '[{"dependencies":[{"status":"closed","labels":["needs-operator"],"dependency_type":"blocks","title":"decide"}]}]' sp-x
wantrc "open_ask_blocker: a closed ask dep is not a blocker" 1 $?

open_ask_blocker '[{"dependencies":[{"status":"open","labels":["plan"],"dependency_type":"blocks","title":"decide"}]}]' sp-x
wantrc "open_ask_blocker: an open blocks dep with no ask label is not a blocker" 1 $?

# sp-2a4hd: this bead's own gh-closeout ask must not decision-block the reopened bead
# it is itself waiting to close.
open_ask_blocker '[{"dependencies":[{"status":"open","labels":["needs-operator"],"dependency_type":"blocks","title":"Close GitHub issue 5 for bead sp-x"}]}]' sp-x
wantrc "open_ask_blocker: this bead's own gh-closeout ask is NOT its own blocker (sp-2a4hd)" 1 $?

open_ask_blocker '[{"dependencies":[{"status":"open","labels":["needs-operator"],"dependency_type":"blocks","title":"Close GitHub issue 5 for bead sp-OTHER"}]}]' sp-x
wantrc "open_ask_blocker: a gh-closeout ask for a DIFFERENT bead IS still a blocker" 0 $?

open_ask_blocker '' sp-x
wantrc "open_ask_blocker: unparseable JSON fails closed (not blocked, proceeds)" 1 $?

tl_summary
