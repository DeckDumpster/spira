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
#      aeon.sh certifies the branch itself (queue.sh submit) and the bead stays closed with
#      landstate CERTIFIED at its tip.
#   4. gate status 3, self-certification comes back red — the bead is REOPENED with the
#      queue.sh submit output as evidence.
#   5. gate status 3, but this exact tip is ALREADY CERTIFIED (the session submitted it
#      itself) — aeon.sh does not re-certify.
#   6. gate status 3, branch has no commits ahead of the base (a delivers-only close) —
#      nothing to certify, no queue.sh call is made.
#   7. gate-run.sh ITSELF (not the stub): a verdict recorded for one tree, read back after
#      the base moved and the branch was rebased onto it, answers status 4 — never the
#      same 3 it would answer for a branch that was never gated at all (sp-7uah8).
#   8. gate status 4 through aeon.sh's own routing (the stub): self-certifies exactly as
#      status 3 does, but the note names the stale verdict and never claims no gate ran.
#
# covers: spira/aeon.sh spira/gate-run.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

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
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-gate-close-silent: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }

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
            mkdir -p "$SPIRA_RUN/landstate"
            printf 'CERTIFIED %s %s ' "$tip" "$(date +%s)" > "$SPIRA_RUN/landstate/${br#spira/}"
            printf 'queue.sh submit: certified %s (stub)\n' "$br"
            exit 0
        else
            printf 'queue.sh submit: %s failed the gate (stub-red)\n' "$br" >&2
            exit 1
        fi
        ;;
esac
STUB
chmod +x "$SPIRA_HOME/queue.sh"

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
if [ "$BEAD_ID" = sp-cert-already ]; then
    tip="$(git rev-parse HEAD)"
    mkdir -p "$SPIRA_RUN/landstate"
    printf 'CERTIFIED %s %s ' "$tip" "$(date +%s)" > "$SPIRA_RUN/landstate/$BEAD_ID"
fi
BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" close "$BEAD_ID" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

run_aeon() {
    local gate_code="$1" queue_code="${2:-0}"
    rm -rf "$SPIRA_RUN/worktree"
    printf '%s\n' "$gate_code" > "$TMP/gate-status-code"
    printf '%s\n' "$queue_code" > "$TMP/queue-submit-code"
    "$HERE/aeon.sh" builder > "$TMP/out" 2>&1
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
want "st=0: bead is left closed, not reopened" "closed" "$(bead_status sp-gcs-0)"

# ======================================================================================
echo
echo "POSITIVE CONTROL — st=2 (gate still running) writes a note (must pass in both trees):"
# ======================================================================================
fresh; seed sp-gcs-1
run_aeon 2
want "st=2: note mentions 'still running'" "still running" "$(bead_notes sp-gcs-1)"
want "st=2: log mentions gate still running" "closed with its gate still running" \
    "$(cat "$TMP/out" 2>/dev/null)"
want "st=2: bead is left closed" "closed" "$(bead_status sp-gcs-1)"

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

# ======================================================================================
echo
echo "st=3, branch ahead of base, nothing CERTIFIED yet — aeon.sh self-certifies (sp-u9f82):"
# ======================================================================================
fresh; seed sp-cert-ok
run_aeon 3 0
want "self-cert PASS: note mentions 'Certified by aeon.sh'" "Certified by aeon.sh" "$(bead_notes sp-cert-ok)"
want "self-cert PASS: log mentions certified" "certified by aeon.sh" "$(cat "$TMP/out" 2>/dev/null)"
want "self-cert PASS: bead is left closed" "closed" "$(bead_status sp-cert-ok)"
tip="$(git -C "$REPO" rev-parse spira/sp-cert-ok 2>/dev/null)"
want "self-cert PASS: landstate is CERTIFIED at the branch tip" "CERTIFIED $tip" \
    "$(cat "$SPIRA_RUN/landstate/sp-cert-ok" 2>/dev/null)"

# ======================================================================================
echo
echo "st=3, self-certification comes back red — the bead is reopened with the evidence:"
# ======================================================================================
fresh; seed sp-cert-red
run_aeon 3 1
want "self-cert RED: note mentions self-certification failure" "queue.sh submit) and it failed" "$(bead_notes sp-cert-red)"
want "self-cert RED: log mentions REOPENED" "REOPENED — self-certification failed" \
    "$(cat "$TMP/out" 2>/dev/null)"
want "self-cert RED: bead is reopened" "open" "$(bead_status sp-cert-red)"

# ======================================================================================
echo
echo "st=3, this tip is already CERTIFIED — no re-certification:"
# ======================================================================================
fresh; seed sp-cert-already
run_aeon 3 0
want "already-certified: log says so" "already CERTIFIED — the session submitted it itself" \
    "$(cat "$TMP/out" 2>/dev/null)"
nowant "already-certified: queue.sh submit was never called for it" "spira/sp-cert-already" \
    "$(cat "$TMP/queue-calls.log" 2>/dev/null)"
want "already-certified: bead is left closed" "closed" "$(bead_status sp-cert-already)"

# ======================================================================================
echo
echo "st=3, nothing committed (delivers-only close) — nothing to certify:"
# ======================================================================================
fresh; seed_delivers sp-cert-nocommit
run_aeon 3 0
want "nothing-ahead: log says nothing to certify" "nothing to certify" "$(cat "$TMP/out" 2>/dev/null)"
nowant "nothing-ahead: queue.sh submit was never called" "spira/sp-cert-nocommit" \
    "$(cat "$TMP/queue-calls.log" 2>/dev/null)"
want "nothing-ahead: bead is left closed" "closed" "$(bead_status sp-cert-nocommit)"

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

out1="$(bash "$HERE/gate-run.sh" --status "$BR2" fixture 2>&1)"; rc1=$?
is "hand-built key matches gate-run.sh's own — recorded PASS answers 0" "0" "$rc1"

# A landing elsewhere moves the base, then this branch is rebased onto it — exactly the
# sequence aeon.sh's own post-close rebase performs on a branch that was left behind.
git -C "$REPO" checkout -q main
printf 'h\n' > "$REPO/h"; git -C "$REPO" add h
git -C "$REPO" commit -qm "unrelated landing" >/dev/null
git -C "$REPO" push -q origin main
git -C "$REPO" checkout -q "$BR2"
git -C "$REPO" rebase -q origin/main >/dev/null

out2="$(bash "$HERE/gate-run.sh" --status "$BR2" fixture 2>&1)"; rc2=$?
is "rebased out from under a recorded PASS — status answers 4, not 3" "4" "$rc2"
want "st=4: message names a different tree, not silence" "different tree" "$out2"

# POSITIVE CONTROL — a branch that was truly never gated still answers 3, so 4 is not
# just 3 renamed everywhere.
BR3="spira/sp-gcs-nevergated"
git -C "$REPO" branch -q "$BR3" origin/main 2>/dev/null || true
out3="$(bash "$HERE/gate-run.sh" --status "$BR3" fixture 2>&1)"; rc3=$?
is "never gated: status still answers 3" "3" "$rc3"

# ======================================================================================
echo
echo "st=4 through aeon.sh's own routing (the stub): self-certifies like st=3, but the"
echo "note names the stale verdict and never claims no gate ran (sp-7uah8):"
# ======================================================================================
fresh; seed sp-gcs-stale4
run_aeon 4 0
want "st=4: note names the stale verdict as the reason, not a missing gate" \
    "stopped applying" "$(bead_notes sp-gcs-stale4)"
nowant "st=4: note must never claim no gate ran" "no gate ran" "$(bead_notes sp-gcs-stale4)"
want "st=4: log mentions certifying after a stale verdict" \
    "certified by aeon.sh after a stale gate verdict" "$(cat "$TMP/out" 2>/dev/null)"
want "st=4: bead is left closed" "closed" "$(bead_status sp-gcs-stale4)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
