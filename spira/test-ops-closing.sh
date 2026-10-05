#!/usr/bin/env bash
#
# test-ops-closing.sh — the close-time fences on an aeon's finished session.
#
#   ./test-ops-closing.sh
#
# Driven through the REAL aeon against a real bd on a throwaway fixture, with a shim
# standing in for the model: a builder's hand-on standing as SUBMITTED, and the retired SOP
# closing rule's key being ignored with a warning.
#
# THE CLOSE-REASON FENCE'S AEON ROWS ARE GONE (sp-v62vn). The fence lives in the verdict's
# closed branch (aeon verdict.rs), and every session is restricted now and hands its bead on
# only through the work verbs, so decide::builder_closed is false for every session and the
# fence is reached by none. Its T3 rows asserted that unreachable path and are deleted, not
# rewritten; UC-aeon-execution-16 is marked uncovered in docs/test-plan/aeon-execution.toml.
# The T1 table over close-reason-flags.py stays: lib.sh's detect_invalid_closed shares it.
#
# WHAT "SILENCE" MEANS, EXACTLY, and why the distinction is the entire suite. A session may
# end three honest ways, and each is one command:
#
#   nothing on the shelf fit; something new was diagnosed   sop write
#   an SOP fit but was incomplete                           sop write   (the upsert)
#   an SOP fit and its CHECK confirmed                      sop applied --check pass
#
# The third row is what keeps this from firing on the good case. "It matched, it held, it
# taught us nothing new" is the outcome a healthy shelf produces most of the time and is
# creditable; a check that poisoned it would punish the sessions the design wants. So every
# case here is a PAIR (law-absence-needs-a-positive-control): a silent session is required to
# poison, and each honest ending is required NOT to — including the two that are merely
# cheap. A check that never fires and a check that always fires look the same from a report
# that only ever contains one of them.
#
# THE FOUR EDGES WORTH NAMING, because each is a way the rule could go wrong in the direction
# nobody would notice:
#
#   --held no       satisfies. It is an honest outcome of a runbook that genuinely fitted,
#                   and demanding an amendment on top of it would make `--held yes` the
#                   cheapest exit — a lie, in the one field the shelf is measured by.
#   --check fail    does NOT satisfy. That is the session's own statement that nothing on the
#                   shelf applied, which is row one, and row one's exit is a write.
#   a retirement    does NOT satisfy. Removing a runbook is curation; it is not the thing the
#                   incident was supposed to leave behind.
#   an OLD record   does not satisfy a LATER session. The question is what this session
#                   recorded, not whether the bead has ever been recorded against.
#
# AND IT DECLINES TO JUDGE WHAT IT CANNOT READ. An unreadable ledger is not an absence, and
# treating one as an absence would poison every incident closed on the day the database is
# down — the day a runbook is worth most.
#
# THE PERSONA IS A FIXTURE NAMED SOMETHING ELSE, on purpose. The rule is declared by a fayth
# (FAYTH_SOP_REQUIRED) rather than keyed on the string "ops" in aeon.sh, so the suite pins a
# non-default: a healer that is not called ops is still held to the rule, and a builder is
# not. Asserting through the shipped ops.fayth would pass just as well against a check with
# the persona's name written into it.
#
# Driven through the REAL aeon and the REAL sop against a real bd on a throwaway
# fixture, with a shim standing in for the model. What is under test is a comparison of two
# database reads either side of a session, and a stub of either side would be a second
# implementation of the thing in question (law-prefer-the-real-dependency).
#
# defect: sp-9pyr
# tier: T3
# covers: aeon/src/* sop/src/*.rs spira/close-reason-flags.py spira/chamber/ops.fayth spira/chamber/ops.md spira/test-ops-closing.sh
# covers: aeon/src/* spira/close-reason-flags.py spira/chamber/ops.fayth spira/test-ops-closing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-ops-closing.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-ops-closing
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up opsclosing || { echo "test-ops-closing: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

HOMEDIR="$TMP/home"; mkdir -p "$HOMEDIR/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$HERE/close-reason-flags.py" "$HOMEDIR/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$HOMEDIR/"
cp -r "$HERE/actors" "$HOMEDIR/" 2>/dev/null || true
RUN="$TMP/run"; mkdir -p "$RUN"
REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$REPO_MAP"

# A FAYTH NOT CALLED ops, so nothing here can key on the persona's name.
for f in builder; do
    cat > "$HOMEDIR/chamber/$f.fayth" <<FAYTH
FAYTH_NAME=$f
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
    printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' \
        > "$HOMEDIR/chamber/$f.md"
done

# THE SHIM IS THE SESSION. It always commits and always closes, so the commit half of the
# verdict is satisfied in every case here and the only thing under test is the verdict. The guard is not decoration: a suite shimming
# `claude` by PATH alone would run the real model against a real account if anything reordered PATH.
BIN="$TMP/bin"; mkdir -p "$BIN"
# sp-mve9i: the aeon reads its bead's state from the lifecycle row, never bd status; the
# shim's bd close is told to it in lifecycle terms (testlib.sh lc_aeon_mirror).
lc_aeon_mirror "$TMP/lcm"; export PATH="$TMP/lcm:$PATH"
# aeon and the spira-claim it ranks through are invoked by name on the suite's PATH
# (sp-gypjk); run_aeon's env -i keeps that PATH.
for _t in aeon spira-claim; do
    command -v "$_t" >/dev/null 2>&1 \
        || { echo "test-ops-closing: $_t is not on PATH — refusing to run the real model" >&2; exit 1; }
done
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'my work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"
# The model session is restricted (sp-v62vn); the shim is a fixture — testlib
# aeon_fixture_agent (SPIRA_AGENT below names the wrapper it writes).
aeon_fixture_agent "$BIN/claude"

# THE ENVIRONMENT IS NAMED, NOT INHERITED. Two keys make this mandatory rather than tidy: an
# inherited SPIRA_CONF would let a real box decide these verdicts, and an inherited
# SPIRA_WIKI could write into a real wiki page. HOME is the real one because `bd` and `dolt`
# read their credentials from it, and SPIRA_PATH is passed because conf.sh rebuilds PATH from it.
run_aeon() {             # run_aeon <fayth> <act>
    printf '%s' "$2" > "$TMP/act"
    rm -rf "$RUN/worktree"
    env -i HOME="$HOME" PATH="$BIN:$PATH" SPIRA_PATH="${SPIRA_PATH:-}" TMP="$TMP" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_WIKI="" \
        SPIRA_HOME="$HOMEDIR" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$REPO_MAP" SPIRA_AGENT="$SPIRA_AGENT" \
        SPIRA_SCOPE_LABEL="${SPIRA_SCOPE_LABEL:-}" \
        BEADS_NO_AUTO_IMPORT=1 \
        timeout 300 aeon --home "$HOMEDIR" "$1" > "$TMP/out" 2>&1
}
seed() {                 # seed <id> [extra-label]
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\"${2:+,\"$2\"}"
    printf '{"id":"%s","title":"unit failed","status":"open","issue_type":"bug","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}
field() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }
labels() { bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | tr '\n' ' '; }

fresh() {                # fresh <bead-id> [extra-label] — an empty world with one bead
    testdb_reset
    seed "$1" "${2:-}"
}
# A delivers:action bead is exempt from the submitted conversion, so its close stands.
fresh_incident() { fresh "$1" delivers:action; }

echo
echo "a BUILDER that closed — its close stands as submitted:"
fresh sp-oc-8; run_aeon builder none
# A builder's plain work bead: the session's bd close reads, in the lifecycle stand-in, as the
# builder's submit (sp-v62vn: the restricted session never reaches sp-qsona's bd conversion).
is     "the bead's hand-on stands as SUBMITTED, not undone" SUBMITTED "$(SPIRA_RUN="$RUN" lc_row_state sp-oc-8)"
nowant "it is not poisoned"        "spira-poison" "$(labels sp-oc-8)"
nowant "and a fayth without the key draws no retirement warning" "is retired" "$(cat "$TMP/out")"
nowant "it is not poisoned"        "spira-poison" "$(labels sp-oc-8)"

echo
echo "a retired FAYTH_SOP_REQUIRED=1 is ignored with a warning naming sp-loycl, never honoured:"
printf 'FAYTH_SOP_REQUIRED=1\n' >> "$HOMEDIR/chamber/builder.fayth"
fresh_incident sp-oc-1; run_aeon builder none
is     "a silent close is not undone"        closed "$(field sp-oc-1 status)"
nowant "and is not poisoned"                 "spira-poison" "$(labels sp-oc-1)"
want   "the warning names the key and this bead" "FAYTH_SOP_REQUIRED is retired (sp-loycl)" "$(cat "$TMP/out")"
nowant "and no closing rule ran"             "closing-rule" "$(cat "$TMP/out")"

echo
echo "T1: close-reason-flags.py, unit directly — no aeon run, no bd"
# ===========================================================================================
# THE FENCE AND detect_invalid_closed IN lib.sh SHARE THIS FILE so they cannot disagree
# about which phrase triggers a refusal. Testing it directly, rather than only through a
# full aeon run per case, is what "already standalone" is for.
crf() {   # crf <reason> -> sets CRF_RC and CRF_OUT (the matched phrase, if any)
    CRF_OUT="$(python3 "$HERE/close-reason-flags.py" "$1" 2>/dev/null)"; CRF_RC=$?
}

crf "done"
wantrc "a clean reason: no match" 1 "$CRF_RC"

crf "TEMPORARY WORKAROUND: x is set until y lands"
wantrc "a plain admit matches" 0 "$CRF_RC"
is     "and names the matched phrase" "TEMPORARY WORKAROUND" "$CRF_OUT"

crf 'pair added — bad-reason ("TEMPORARY WORKAROUND") is reopened; clean reason stays closed'
wantrc "a double-quoted mention is masked, not an admission" 1 "$CRF_RC"

crf "the thing (a TEMPORARY WORKAROUND) was avoided entirely"
wantrc "a parenthetical mention is masked too" 1 "$CRF_RC"

crf 'the `TEMPORARY WORKAROUND` phrase, avoided'
wantrc "a backtick-quoted mention is masked too" 1 "$CRF_RC"

crf "see close-reason-flags.py for the TEMPORARY WORKAROUND check"
wantrc "a line naming this file is a mention, not an admission" 1 "$CRF_RC"

crf "PERMANENT FIX NEEDED here"
wantrc "PERMANENT FIX NEEDED matches" 0 "$CRF_RC"

crf "mitigated-only for now"
wantrc "mitigated-only matches" 0 "$CRF_RC"

crf "TODO: fix this properly"
wantrc "a bare TODO matches" 0 "$CRF_RC"

crf "temporarily fixed the issue for the release"
wantrc "temporarily fixed matches" 0 "$CRF_RC"

crf "a TEMPORARY workaround was used, in lowercase"
wantrc "matching is case-insensitive" 0 "$CRF_RC"

tl_summary
