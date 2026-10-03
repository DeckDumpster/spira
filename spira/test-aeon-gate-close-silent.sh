#!/usr/bin/env bash
#
# test-aeon-gate-close-silent.sh — a session that closes its bead while the gate has no
#                                   verdict (exit 3) or a FAIL verdict (exit 1) must not leave
#                                   the branch stranded: uncertifiable, unlanded, and silent.
#
# THE ORIGINAL DEFECT. gate_unfinished() returns 0 only when gate-run.sh --status exits 2
# (still running). At the closed-bead teardown path, exit codes 1 (FAIL) and 3 (no gate
# ran) fell through with a note and nothing else — the close looked identical to one that
# had obtained a passing verdict, and NOTHING ever certified or reopened it.
#
# THE FOLLOW-ON DEFECT (sp-u9f82). Even once st=3 was noted, nothing acted on it: whether a
# closed, committed branch ever reached the queue depended on whether the model, inside its
# own turn, happened to run queue.sh submit itself. Two sessions could close identically and
# diverge — one CERTIFIED because it self-submitted, the other stranded with no landstate at
# all, because the harness never certified on the session's behalf.
#
# WHAT IS TESTED:
#   0. gate status 0 (a recorded PASS for this exact tree) writes a note naming the PASS
#      and quoting gate-run.sh's own key and covered-suite list, and does not reopen the
#      bead (sp-0pk2x: this status fell through the case entirely — 73 of 76 cert-gate-red
#      reopens held exactly this status at close time with no note at all).
#   1. POSITIVE CONTROL — gate status 2 (still running) writes a note, passes in both trees.
#   2. gate status 1 (FAIL verdict already on record for this tree) — REOPENS the bead with
#      the recorded evidence, rather than leaving it for a later pass to rediscover.
#   3. gate status 3 (no gate ever ran), branch ahead of base, no existing CERTIFIED record —
#      aeon.sh does NOT certify the branch itself (sp-dv6ae: queue.sh submit runs gate.sh,
#      which can block this session on host-wide admission for as long as every slot is
#      busy, holding its fleet slot the whole time). The note says certification is handed
#      to the landing pass, no landstate is written, queue.sh is never called, and the bead
#      still ends open+spira-submitted (sp-qsona: certification proceeds from submitted;
#      only the landing pass closes a work bead).
#   4. gate status 3, but this exact tip is ALREADY CERTIFIED (the session submitted it
#      itself, or an earlier pass did) — aeon.sh does not defer and never calls queue.sh.
#   5. gate status 3, branch has no commits ahead of the base (a delivers-only close) —
#      nothing to certify, no queue.sh call is made.
#   6. gate-run.sh ITSELF (not the stub): a verdict recorded for one tree, read back after
#      the base moved and the branch was rebased onto it, answers status 4 — never the
#      same 3 it would answer for a branch that was never gated at all (sp-7uah8).
#   7. gate status 4 through aeon.sh's own routing (the stub): defers exactly as status 3
#      does, but the note names the stale verdict and never claims no gate ran.
#   8. gate-run.sh ITSELF: a run that started and then died without ever recording a verdict
#      answers 5, never 1 — 1 must mean a real recorded FAIL from gate.sh, not a corpse with
#      no rc file (sp-k7klr: this collapse is what let sp-5dcpj be reopened as cert-gate-red
#      20+ times against one dead run, with no code change in between).
#   9. gate status 5 through aeon.sh's own routing (the stub): defers exactly as status 3
#      does, and the note never claims a recorded FAIL verdict.
#  10. gate status 3 with a branch whose own spira/build-fence.sh is red — the in-session fast
#      tier refuses the handoff: the bead is reopened plain (not submitted) with the failure
#      text. The same branch with a green fence is still handed off (positive control).
#
# SUBMITTED, NOT CLOSED (sp-qsona). Every case whose close is not undone by a reopen ends
# open carrying spira-submitted — the conversion runs AFTER the gate/defer branch, so a
# reopen there (a FAIL verdict already on record, st=1) leaves the bead plain open and
# claimable instead. The delivers:action case is exempt from the conversion and stays closed.
#
# tier: T2
# covers: aeon/src/* spira/gate-run.sh UC-aeon-execution-15
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-gate-close-silent
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeongcls || { echo "test-aeon-gate-close-silent: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null
export REPO

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
# conf.d IS COPIED IN (matching test-aeon-sweep.sh, test-aeon-world-stop.sh, ...): aeon's
# own in-process config registry (spira_config::resolve, aeon::conf::merge_resolved_config)
# derives conf.d from THIS --home and now REFUSES to start if it is missing (sp-1cdgq) --
# a --home with no conf.d used to resolve silently to nothing instead of refusing.
cp -r "$HERE/conf.d" "$SPIRA_HOME/"
printf '. "%s/lib.sh"\n' "$HERE" > "$SPIRA_HOME/lib.sh"   # the aeon binary sources <home>/lib.sh; this is the real one, as aeon.sh sourced it
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | queue | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
command -v aeon >/dev/null 2>&1 \
    || { echo "test-aeon-gate-close-silent: aeon is not on PATH — refusing to run the real model" >&2; exit 1; }

# gate-run.sh stub: exits with the code written to $TMP/gate-status-code
cat > "$SPIRA_HOME/gate-run.sh" <<'STUB'
#!/usr/bin/env bash
code="$(cat "${TMP}/gate-status-code" 2>/dev/null)"; code="${code:-2}"
case "$code" in
    0) echo "gate-run: PASSED fixture in fixture after 10s"
       echo "gate-run: key deadbeef cafefeed"
       echo "gate-run: gate PASS covered suites: test-fixture.sh" ;;
    2) echo "gate-run: still running for fixture — 10s so far, pid $$" ;;
    1) echo "gate-run: FAILED fixture in fixture after 10s" ;;
    4) echo "gate-run: the recorded verdict for fixture is for a different tree — key was oldtip oldbase, now newtip newbase. A rebase, a new commit, or the base moving invalidated it." ;;
    5) echo "gate-run: the gate run for fixture died after 10s without recording a verdict" ;;
esac
exit "$code"
STUB
chmod +x "$SPIRA_HOME/gate-run.sh"

# queue.sh stub: only `submit <branch> <repo-name>`, controlled by $TMP/queue-submit-code.
# Records every call (branch + repo) so a test can assert self-certification was, or was
# not, attempted. A "pass" mimics land_mark CERTIFIED (lib.sh's own file format) directly,
# since this stub stands in for the real queue.sh -> gate.sh -> land_mark chain.
cat > "$SPIRA_HOME/queue.sh" <<'STUB'
#!/usr/bin/env bash
printf '%s %s\n' "${1:-}" "${2:-}" >> "${TMP}/queue-calls.log"
code="$(cat "${TMP}/queue-submit-code" 2>/dev/null)"; code="${code:-0}"
case "${1:-}" in
    submit)
        br="${2:-}"
        if [ "$code" = 0 ]; then
            tip="$(git -C "$REPO" rev-parse "$br" 2>/dev/null)"
            mkdir -p "$SPIRA_RUN/lc-row"
            printf 'CERTIFIED %s\n' "$tip" > "$SPIRA_RUN/lc-row/${br#spira/}"
            printf 'queue.sh submit: certified %s (stub)\n' "$br"
            exit 0
        else
            printf 'queue.sh submit: %s failed the gate (stub-red)\n' "$br" >&2
            case "$code" in
                75) printf 'gate: VERDICT=NO_VERDICT reason=harness-fault branch=%s repo=fixture suite=-\n' "$br" >&2 ;;
                76) printf 'gate: VERDICT=BASE_FAIL reason=base-red branch=%s repo=fixture suite=test-fixture.sh\n' "$br" >&2 ;;
            esac
            exit "$code"
        fi
        ;;
esac
STUB
chmod +x "$SPIRA_HOME/queue.sh"
ln -sf queue.sh "$SPIRA_HOME/queue"   # the queue binary replaced queue.sh; this stub stands in for both, by name
# spira-lc stub: `show` answers the lifecycle row recorded in $SPIRA_RUN/lc-row/<id> ("<state> <tip>").
cat > "$SPIRA_HOME/spira-lc" <<'STUB'
#!/usr/bin/env bash
case "$1" in
    show) if [ -f "$SPIRA_RUN/lc-row/$2" ]; then read -r st tip < "$SPIRA_RUN/lc-row/$2"
          printf '{"bead":{"bead_id":"%s","state":"%s","tip":"%s","version":"1"},"delivery":null}\n' "$2" "$st" "$tip"
          else printf '{"bead":{"bead_id":"%s","state":"WORKING","version":"1"},"delivery":null}\n' "$2"; fi ;;
    *) exit 0 ;;
esac
STUB
chmod +x "$SPIRA_HOME/spira-lc"
# The aeon runs with the fixture home FIRST on PATH, so these stubs shadow the tree's tools.
printf '#!/usr/bin/env bash\nexit 0\n' > "$SPIRA_HOME/spira-lint"; chmod +x "$SPIRA_HOME/spira-lint"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}

# A bead that will close via delivers:action rather than a commit — used for the
# nothing-ahead-of-base case, where there is nothing for aeon.sh to certify.
seed_delivers() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\",\"delivers:action\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}

# A successor bead, already closed — the target of `bd supersede --with`.
seed_closed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"closed","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}

# The shim commits work and closes the bead, simulating a session that did the work and
# closed without ever running its own gate. sp-cert-nocommit closes with nothing committed
# (delivers:action carries the evidence). sp-cert-already also writes a CERTIFIED landstate
# record for its own tip before closing, simulating a session that called queue.sh submit
# itself.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
case "$BEAD_ID" in
    sp-cert-nocommit) ;;
    *)
        printf 'work\n' >> f
        git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$BEAD_ID — done" >/dev/null 2>&1
        ;;
esac
case "$BEAD_ID" in
    sp-cert-fence-red|sp-cert-fence-ok)
        mkdir -p spira
        if [ "$BEAD_ID" = sp-cert-fence-red ]; then
            printf '#!/usr/bin/env bash\necho "build-fence: make build FAILED (fixture)" >&2\nexit 1\n' > spira/build-fence.sh
        else
            printf '#!/usr/bin/env bash\nexit 0\n' > spira/build-fence.sh
        fi
        git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$BEAD_ID — fence" >/dev/null 2>&1
        ;;
esac
if [ "$BEAD_ID" = sp-cert-already ]; then
    tip="$(git rev-parse HEAD)"
    mkdir -p "$SPIRA_RUN/lc-row"
    printf 'CERTIFIED %s\n' "$tip" > "$SPIRA_RUN/lc-row/$BEAD_ID"
fi
if [ "$BEAD_ID" = sp-cert-super ]; then
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" supersede "$BEAD_ID" --with sp-cert-super-succ >/dev/null 2>&1
else
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" close "$BEAD_ID" --reason "done" >/dev/null 2>&1
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

run_aeon() {
    local gate_code="$1" queue_code="${2:-0}"
    rm -rf "$SPIRA_RUN/worktree"
    printf '%s\n' "$gate_code" > "$TMP/gate-status-code"
    printf '%s\n' "$queue_code" > "$TMP/queue-submit-code"
    PATH="$SPIRA_HOME:$PATH" aeon --home "$SPIRA_HOME" builder > "$TMP/out" 2>&1
}

bead_notes() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("notes", "") or "")' 2>/dev/null
}

bead_status() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status", "") or "")' 2>/dev/null
}

bead_labels() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(",".join(d[0].get("labels") or []))' 2>/dev/null
}

fresh() { testdb_reset; : > "$TMP/queue-calls.log"; }

echo "test-aeon-gate-close-silent.sh"

# ======================================================================================
echo
echo "st=0 (recorded PASS for this exact tree) writes a note naming key + covered suites (sp-0pk2x):"
# ======================================================================================
fresh; seed sp-gcs-0
run_aeon 0
want "st=0: note mentions 'gate PASS covered suites'" "gate PASS covered suites" "$(bead_notes sp-gcs-0)"
want "st=0: note names the covered suite" "test-fixture.sh" "$(bead_notes sp-gcs-0)"
want "st=0: note names the (tip, base) key" "key deadbeef cafefeed" "$(bead_notes sp-gcs-0)"
want "st=0: log mentions the recorded PASS" "closed with a recorded PASS gate verdict" \
    "$(cat "$TMP/out" 2>/dev/null)"
is   "st=0: bead is not reopened — converted to submitted" "open" "$(bead_status sp-gcs-0)"
want "st=0: carrying the submitted label" "spira-submitted" "$(bead_labels sp-gcs-0)"

# ======================================================================================
echo
echo "POSITIVE CONTROL — st=2 (gate still running) writes a note (must pass in both trees):"
# ======================================================================================
fresh; seed sp-gcs-1
run_aeon 2
want "st=2: note mentions 'still running'" "still running" "$(bead_notes sp-gcs-1)"
want "st=2: log mentions gate still running" "closed with its gate still running" \
    "$(cat "$TMP/out" 2>/dev/null)"
is   "st=2: bead is converted to submitted, not left closed" "open" "$(bead_status sp-gcs-1)"
want "st=2: carrying the submitted label" "spira-submitted" "$(bead_labels sp-gcs-1)"

# ======================================================================================
echo
echo "st=1 (FAIL verdict already on record) — reopened with the recorded evidence:"
# ======================================================================================
fresh; seed sp-gcs-3
run_aeon 1
want "st=1: note mentions 'FAIL gate verdict'" "FAIL gate verdict" "$(bead_notes sp-gcs-3)"
want "st=1: log mentions REOPENED" "REOPENED — closed against a recorded FAIL gate verdict" \
    "$(cat "$TMP/out" 2>/dev/null)"
want "st=1: bead is reopened, not left closed" "open" "$(bead_status sp-gcs-3)"
nowant "st=1: and NOT marked submitted — it must be claimable again" "spira-submitted" "$(bead_labels sp-gcs-3)"

# ======================================================================================
echo
echo "st=3, branch ahead of base, nothing CERTIFIED yet — aeon.sh defers to the landing pass"
echo "instead of self-certifying (sp-dv6ae, replacing sp-u9f82's self-certify-here fix):"
# ======================================================================================
fresh; seed sp-cert-ok
run_aeon 3 0
want "defer: note hands certification to the landing pass" "handed to the landing pass" "$(bead_notes sp-cert-ok)"
nowant "defer: note never claims aeon.sh certified it" "Certified by aeon.sh" "$(bead_notes sp-cert-ok)"
want "defer: log mentions handing certification to the landing pass" "handed to the landing pass" "$(cat "$TMP/out" 2>/dev/null)"
is   "defer: bead is converted to submitted, not left closed" "open" "$(bead_status sp-cert-ok)"
want "defer: carrying the submitted label" "spira-submitted" "$(bead_labels sp-cert-ok)"
nowant "defer: queue.sh submit was never called — no blocking self-cert" "spira/sp-cert-ok" \
    "$(cat "$TMP/queue-calls.log" 2>/dev/null)"
tip="$(git -C "$REPO" rev-parse spira/sp-cert-ok 2>/dev/null)"
nowant "defer: no landstate is written — certification is the landing pass's to run" "CERTIFIED $tip" \
    "$(cat "$SPIRA_RUN/lc-row/sp-cert-ok" 2>/dev/null)"

# ======================================================================================
echo
echo "st=3, but the close is superseded — never handed to certification, close stands (sp-vjf6u):"
# ======================================================================================
fresh; seed_closed sp-cert-super-succ; seed sp-cert-super
run_aeon 3 0
nowant "superseded: note does not hand certification to the landing pass" "handed to the landing pass" \
    "$(bead_notes sp-cert-super)"
want "superseded: log says the branch is not handed to certification" \
    "not handing spira/sp-cert-super to certification" "$(cat "$TMP/out" 2>/dev/null)"
is   "superseded: bead stays closed, not converted to submitted" "closed" "$(bead_status sp-cert-super)"
nowant "superseded: not carrying the submitted label" "spira-submitted" "$(bead_labels sp-cert-super)"
nowant "superseded: queue.sh submit was never called" "spira/sp-cert-super " \
    "$(cat "$TMP/queue-calls.log" 2>/dev/null)"
tip="$(git -C "$REPO" rev-parse spira/sp-cert-super 2>/dev/null)"
nowant "superseded: no landstate is written" "CERTIFIED $tip" \
    "$(cat "$SPIRA_RUN/lc-row/sp-cert-super" 2>/dev/null)"

# ======================================================================================
echo
echo "st=3, this tip is already CERTIFIED — no deferral needed, no re-certification:"
# ======================================================================================
fresh; seed sp-cert-already
run_aeon 3 0
want "already-certified: log says so" "already CERTIFIED — the session submitted it itself" \
    "$(cat "$TMP/out" 2>/dev/null)"
nowant "already-certified: queue.sh submit was never called for it" "spira/sp-cert-already" \
    "$(cat "$TMP/queue-calls.log" 2>/dev/null)"
is   "already-certified: bead is converted to submitted, not left closed" "open" "$(bead_status sp-cert-already)"
want "already-certified: carrying the submitted label" "spira-submitted" "$(bead_labels sp-cert-already)"

# ======================================================================================
echo
echo "st=3, nothing committed (delivers-only close) — nothing to certify:"
# ======================================================================================
fresh; seed_delivers sp-cert-nocommit
run_aeon 3 0
want "nothing-ahead: log says nothing to certify" "nothing to certify" "$(cat "$TMP/out" 2>/dev/null)"
nowant "nothing-ahead: queue.sh submit was never called" "spira/sp-cert-nocommit" \
    "$(cat "$TMP/queue-calls.log" 2>/dev/null)"
want "nothing-ahead: bead is left closed (delivers: exempts it from the submitted conversion)" "closed" "$(bead_status sp-cert-nocommit)"
nowant "nothing-ahead: no submitted label" "spira-submitted" "$(bead_labels sp-cert-nocommit)"

# ======================================================================================
echo
echo "gate-run.sh itself (not the stub): a verdict recorded for one tree answers 4, not"
echo "3, once the base moves and the branch is rebased onto it — the two conditions"
echo "gate-run.sh --status used to collapse into one exit code (sp-7uah8):"
# ======================================================================================
BR2="spira/sp-gcs-stalekey"
git -C "$REPO" fetch -q origin
git -C "$REPO" checkout -q -B "$BR2" origin/main >/dev/null 2>&1
printf 'g\n' > "$REPO/g"; git -C "$REPO" add g
git -C "$REPO" commit -qm "sp-gcs-stalekey work" >/dev/null
tip1="$(git -C "$REPO" rev-parse "$BR2")"
base1="$(git -C "$REPO" rev-parse origin/main)"

slug2="$(printf '%s.%s' fixture "$BR2" | tr -c 'A-Za-z0-9._-' '_')"
D2="$SPIRA_RUN/gate-run/$slug2"
mkdir -p "$D2"
printf '0' > "$D2/rc"
printf '%s %s' "$tip1" "$base1" > "$D2/key"
date +%s > "$D2/started"
: > "$D2/out"

out1="$(gate-run.sh --status "$BR2" fixture 2>&1)"; rc1=$?
is "hand-built key matches gate-run.sh's own — recorded PASS answers 0" "0" "$rc1"

# A landing elsewhere moves the base, then this branch is rebased onto it — exactly the
# sequence aeon.sh's own post-close rebase performs on a branch that was left behind.
git -C "$REPO" checkout -q main
printf 'h\n' > "$REPO/h"; git -C "$REPO" add h
git -C "$REPO" commit -qm "unrelated landing" >/dev/null
git -C "$REPO" push -q origin main
git -C "$REPO" checkout -q "$BR2"
git -C "$REPO" rebase -q origin/main >/dev/null

out2="$(gate-run.sh --status "$BR2" fixture 2>&1)"; rc2=$?
is "rebased out from under a recorded PASS — status answers 4, not 3" "4" "$rc2"
want "st=4: message names a different tree, not silence" "different tree" "$out2"

# POSITIVE CONTROL — a branch that was truly never gated still answers 3, so 4 is not
# just 3 renamed everywhere.
BR3="spira/sp-gcs-nevergated"
git -C "$REPO" branch -q "$BR3" origin/main 2>/dev/null || true
out3="$(gate-run.sh --status "$BR3" fixture 2>&1)"; rc3=$?
is "never gated: status still answers 3" "3" "$rc3"

# ======================================================================================
echo
echo "st=4 through aeon.sh's own routing (the stub): defers like st=3, but the note names"
echo "the stale verdict and never claims no gate ran (sp-7uah8):"
# ======================================================================================
fresh; seed sp-gcs-stale4
run_aeon 4 0
want "st=4: note names the stale verdict as the reason, not a missing gate" \
    "no longer applies" "$(bead_notes sp-gcs-stale4)"
nowant "st=4: note must never claim no gate ran" "no gate ran" "$(bead_notes sp-gcs-stale4)"
want "st=4: log mentions handing certification to the landing pass" \
    "handed to the landing pass" "$(cat "$TMP/out" 2>/dev/null)"
is   "st=4: bead is not reopened — converted to submitted" "open" "$(bead_status sp-gcs-stale4)"
want "st=4: carrying the submitted label" "spira-submitted" "$(bead_labels sp-gcs-stale4)"
nowant "st=4: queue.sh submit was never called — no blocking self-cert" "spira/sp-gcs-stale4" \
    "$(cat "$TMP/queue-calls.log" 2>/dev/null)"

# ======================================================================================
echo
echo "st=5 (gate run died before recording a verdict) through aeon.sh's own routing (the"
echo "stub): defers exactly like st=3 — and the note never claims a FAIL verdict"
echo "(sp-k7klr: that conflation is the exact bug that reopened sp-5dcpj 20+ times):"
# ======================================================================================
fresh; seed sp-gcs-died5
run_aeon 5 0
want "st=5: note hands certification to the landing pass" "handed to the landing pass" "$(bead_notes sp-gcs-died5)"
nowant "st=5: note must never claim a recorded FAIL verdict" "FAIL gate verdict" "$(bead_notes sp-gcs-died5)"
want "st=5: log mentions handing certification to the landing pass" \
    "handed to the landing pass" "$(cat "$TMP/out" 2>/dev/null)"
is   "st=5: bead is not reopened — converted to submitted" "open" "$(bead_status sp-gcs-died5)"
want "st=5: carrying the submitted label" "spira-submitted" "$(bead_labels sp-gcs-died5)"
nowant "st=5: queue.sh submit was never called — no blocking self-cert" "spira/sp-gcs-died5" \
    "$(cat "$TMP/queue-calls.log" 2>/dev/null)"

# ======================================================================================
echo
echo "gate-run.sh ITSELF (not the stub): a run that started and then died without ever"
echo "recording a verdict answers 5, never 1 — 1 must mean a real recorded FAIL, not a"
echo "corpse with no rc file (sp-k7klr):"
# ======================================================================================
BR4="spira/sp-gcs-died"
git -C "$REPO" branch -q "$BR4" origin/main 2>/dev/null || true
tip4="$(git -C "$REPO" rev-parse "$BR4")"
base4="$(git -C "$REPO" rev-parse origin/main)"

# A pid that is certainly dead by the time gate-run.sh reads it: fork and wait it out.
( : ) & deadpid4=$!; wait "$deadpid4" 2>/dev/null

slug4="$(printf '%s.%s' fixture "$BR4" | tr -c 'A-Za-z0-9._-' '_')"
D4="$SPIRA_RUN/gate-run/$slug4"
mkdir -p "$D4"
printf '%s' "$deadpid4" > "$D4/pid"
printf '%s %s' "$tip4" "$base4" > "$D4/key"
date +%s > "$D4/started"
: > "$D4/out"
# no rc file written — the run died before ever recording one

out4="$(gate-run.sh --status "$BR4" fixture 2>&1)"; rc4=$?
is   "died without a verdict answers 5, never 1" "5" "$rc4"
want "st=5: message says the run died without recording a verdict" "died" "$out4"
nowant "st=5: message must never claim FAILED" "FAILED" "$out4"

# ======================================================================================
echo
echo "st=3 with a red build fence — the handoff is refused in-session; a green fence is"
echo "still handed off (positive control):"
# ======================================================================================
fresh; seed sp-cert-fence-ok
run_aeon 3 0
want "fence green: still handed to the landing pass" "handed to the landing pass" "$(bead_notes sp-cert-fence-ok)"
want "fence green: carrying the submitted label" "spira-submitted" "$(bead_labels sp-cert-fence-ok)"

fresh; seed sp-cert-fence-red
run_aeon 3 0
want "fence red: log says the handoff was refused" "fast tier red, handoff refused" "$(cat "$TMP/out" 2>/dev/null)"
want "fence red: the bead carries the failure text" "make build FAILED (fixture)" "$(bead_notes sp-cert-fence-red)"
nowant "fence red: never handed to the landing pass" "handed to the landing pass" "$(bead_notes sp-cert-fence-red)"
is   "fence red: bead is open, claimable" "open" "$(bead_status sp-cert-fence-red)"
nowant "fence red: NOT marked submitted" "spira-submitted" "$(bead_labels sp-cert-fence-red)"

echo
tl_summary
