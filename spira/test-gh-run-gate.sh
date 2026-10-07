#!/usr/bin/env bash
#
# test-gh-run-gate.sh — gh:run gates replace the awaiting-ci label sweep.
#
#   ./test-gh-run-gate.sh
#
# THE MECHANISM THIS SUITE EXISTS FOR. `awaiting-ci` was a label that told readers "this
# bead is parked on a CI run". Labels are an unvalidated bag — every reader had to remember
# to exclude the label, and a wrong value returned a truthful empty result at read time
# rather than a refusal at write time. A gate makes the bead NOT READY; a reader that knows
# nothing about CI is still correct.
#
# THREE PROPERTIES this suite verifies:
#
# 1. GATE BLOCKS AND UNBLOCKS. A bead with an open gh:run gate is absent from bd ready.
#    After the gate resolves it is present. The positive control — bead in ready BEFORE
#    the gate is created — is the first assertion so a gate that blocks everything and a
#    gate that blocks nothing are both distinguishable here.
#
# 2. bd gate check uses metadata.repo. When a gate carries metadata.repo=org/repo,
#    bd gate check calls `gh run view <id> --repo org/repo`. Two gates, same await_id,
#    different metadata.repo values; a fake gh that succeeds for one and fails for the
#    other: only the matching gate resolves. This is the cross-repo isolation guard.
#    A gate cannot be resolved by a run from another repository.
#
# 3. bd gate discover runs per-repository. The gate-check script iterates pr-mode repos
#    and calls bd gate discover from within each one's directory. A gate whose branch
#    does not exist in the wrong repo is not matched.
#
# WHY EVERY CASE IS A PAIR. Each mechanism fails by doing nothing. A gate check that blocks
# all calls to gh would prevent resolution; one that accepts any call regardless of repo
# would resolve the wrong gates. The same bead is driven both ways so neither failure mode
# passes silently (law-absence-needs-a-positive-control).
#
# A REAL bd BECAUSE WHAT IS ASSERTED IS DATABASE STATE — whether a bead is ready, whether
# a gate is open or closed. A stub would be a second implementation of the thing in question
# (law-prefer-the-real-dependency).
#
# PART 3 (gate-check.sh's own discover-loop argument passing: pr vs push filtering, the
# branch named to `bd gate discover`) retired sp-ubw2o: that decision now lives in the Rust
# `gate-check` crate's `discover_branches` (gate-check/DESIGN.md), covered by `cargo test -p
# gate-check` and the bead's own stubbed-bd smoke run. Parts 1, 2 and 4 stay — they exercise
# `bd`'s own gate mechanism against a real store, and the systemd wiring, neither of which
# this bead touched.
#
# tier: T2
# covers: spira/gate-check.sh sentinel/src/* aeon/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-gh-run-gate
trap 'testdb_drop' EXIT INT TERM
testdb_up gh_run_gate || { echo "test-gh-run-gate: could not build a fixture database"; exit 1; }

B() { timeout 5 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" "$@"; }
ready_ids() { B ready --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(" ".join(i["id"] for i in d if i.get("id")))' 2>/dev/null; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

BASE_PATH="$PATH"

# ======================================================================================
# PART 1: GATE BASICS. A gh:run gate blocks and unblocks a bead through bd alone.
#
# No gh stub needed — gate create and resolve are pure database operations. The positive
# control (bead in ready before the gate) is verified first so a gate that blocks nothing
# reads as wrong here rather than hiding behind the tests that follow.
# ======================================================================================
echo "gate blocks and unblocks a bead:"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":["spira","plan"]}
{"id":"sp-work","title":"do the thing","status":"open","issue_type":"task","labels":["spira","plan"]}
JSONL

# THE POSITIVE CONTROL: bead is in ready before any gate exists.
want "bead is in ready before any gate" "sp-work" "$(ready_ids)"

# CREATE the gate — blocks sp-work.
GATE_ID="$(B gate create --type=gh:run --blocks sp-work --reason "waiting for CI" 2>/dev/null \
    | grep -oP 'sp-\w+' | head -1)"
[ -n "$GATE_ID" ] && ok "gate was created" || bad "gate was created" "no gate id in output"

# GATED BEAD IS ABSENT FROM READY.
nowant "gated bead is absent from bd ready" "sp-work" "$(ready_ids)"

# RESOLVE THE GATE and verify the bead returns.
B gate resolve "$GATE_ID" >/dev/null 2>&1
want "resolved bead returns to bd ready" "sp-work" "$(ready_ids)"

# ======================================================================================
# PART 2: bd gate check uses metadata.repo — cross-repo isolation.
#
# A gate carries metadata.repo so bd gate check calls `gh run view <id> --repo <org/repo>`.
# Two beads, same await_id, different metadata.repo values. A fake gh succeeds for
# org-a/repo-a and fails for org-b/repo-b. Only the org-a gate resolves.
#
# THE FAKE GH IS PLACED FIRST IN PATH. bd gate check uses gh from $PATH, so a shim ahead
# of the real gh intercepts every call without modifying bd or gh. The shim records its
# calls so we can verify metadata.repo became the --repo flag.
# ======================================================================================
echo
echo "bd gate check uses metadata.repo (cross-repo isolation):"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":["spira","plan"]}
{"id":"sp-a","title":"repo-a bead","status":"open","issue_type":"task","labels":["spira","plan"]}
{"id":"sp-b","title":"repo-b bead","status":"open","issue_type":"task","labels":["spira","plan"]}
JSONL

GA="$(B gate create --type=gh:run --blocks sp-a 2>/dev/null | grep -oP 'sp-\w+' | head -1)"
GB="$(B gate create --type=gh:run --blocks sp-b 2>/dev/null | grep -oP 'sp-\w+' | head -1)"

# Set different metadata.repo on each gate — org-a/repo-a vs org-b/repo-b.
B update "$GA" --set-metadata "repo=org-a/repo-a" >/dev/null 2>&1
B update "$GB" --set-metadata "repo=org-b/repo-b" >/dev/null 2>&1

# Set the same await_id on both — a single run ID that the fake gh will recognise only
# for org-a/repo-a.
B update "$GA" --await-id "11111" >/dev/null 2>&1
B update "$GB" --await-id "11111" >/dev/null 2>&1

# FAKE GH: succeeds for org-a/repo-a, fails for org-b/repo-b, and logs every call.
GH_LOG="$TMP/gh-calls.log"
mkdir -p "$TMP/bin"
cat > "$TMP/bin/gh" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GH_LOG"
# run view: succeed only for org-a/repo-a
if [[ "$*" == *"run view"* ]]; then
    if [[ "$*" == *"--repo org-a/repo-a"* ]]; then
        printf '{"status":"completed","conclusion":"success"}\n'
        exit 0
    fi
    exit 1   # any other repo: gh cannot find the run there
fi
printf '[]\n'
SHIM
chmod +x "$TMP/bin/gh"
export GH_LOG

# THE POSITIVE CONTROL: both beads are absent from ready before check.
nowant "sp-a gated before check" "sp-a" "$(ready_ids)"
nowant "sp-b gated before check" "sp-b" "$(ready_ids)"

PATH="$TMP/bin:$BASE_PATH" B gate check --type=gh:run >/dev/null 2>&1

# ONLY THE MATCHING GATE RESOLVES. sp-a (org-a/repo-a) resolves; sp-b (org-b/repo-b) stays.
want   "sp-a resolved: bead returns to ready"    "sp-a" "$(ready_ids)"
nowant "sp-b not resolved: stays blocked"        "sp-b" "$(ready_ids)"

# THE GH CALL CARRIED --repo. The cross-repo guard is in the flag, not in any harness
# code — verify the flag was actually sent so the guard is not silently absent.
gh_calls="$(cat "$GH_LOG" 2>/dev/null)"
want   "check used --repo flag for gate A"         "--repo org-a/repo-a"  "$gh_calls"
want   "check used --repo flag for gate B too"     "--repo org-b/repo-b"  "$gh_calls"

# ======================================================================================
# PART 4: the systemd timer for gate-check exists.
#
# bd gate check must run on a schedule; the bespoke awaiting-ci sweep has been removed.
# An installed unit tells readers the cadence; a missing one means the check never fires.
# ======================================================================================
echo
echo "gate-check timer and service exist:"

SYSTEMD="$HERE/../systemd"
[ -f "$SYSTEMD/spira-gate-check.service" ] \
    && ok "spira-gate-check.service exists" \
    || bad "spira-gate-check.service exists" "file not found"
[ -f "$SYSTEMD/spira-gate-check.timer" ] \
    && ok "spira-gate-check.timer exists" \
    || bad "spira-gate-check.timer exists" "file not found"

# THE CONTENT IS THE CONTRACT. A timer that references the wrong service name installs
# but never fires — asserting on the file's content is the one check that catches that.
if [ -f "$SYSTEMD/spira-gate-check.timer" ]; then
    want "timer references gate-check service" "spira-gate-check" \
         "$(cat "$SYSTEMD/spira-gate-check.timer")"
fi
if [ -f "$SYSTEMD/spira-gate-check.service" ]; then
    want "service ExecStart names gate-check.sh" "gate-check.sh" \
         "$(cat "$SYSTEMD/spira-gate-check.service")"
fi
tl_summary
