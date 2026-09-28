#!/usr/bin/env bash
# test-lifecycle-enforce-gate.sh — whether an aeon takes the lifecycle semantic layer is
#   lifecycle_enforce, an explicit config decision, never "work/spira-lc happen to exist".
#
# THE DEFECT THIS REPRODUCES (sp-74gzo). aeon.sh used to gate on `[ -x "$SPIRA_LC_BIN" ]` /
# `[ -x "$SPIRA_WORK_BIN" ]` alone, and conf.sh auto-resolves both from
# $SPIRA_REPO/target/release whenever unset. Any tree that has simply built the workspace —
# a --with-bins corpus run, a production checkout after `round.sh land` — flipped every aeon
# fixture onto the restricted path, whether or not the operator meant to cut over.
#
# SEEN RED FIRST: before the fix, case A below (binaries built, flag unset) takes the
# restricted path — the stub SPIRA_LC_BIN/SPIRA_WORK_BIN are invoked and their marker files
# appear — because the old gate only asked "are these executable", and stub scripts are.
#
# tier: T1
# covers: spira/aeon.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1090
. "$HERE/testlib.sh"
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-lifecycle-enforce-gate
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up lifecycleenforce || { bail "could not build fixture database"; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan}"
FAYTH_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || bail "aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model"

# STAND-INS FOR "THE BINARIES ARE BUILT". This suite proves the GATE, not lc_claim_bead's own
# correctness (test-aeon-lifecycle-cutover.sh covers that end to end against a real spira-lc
# server) — an executable script that records its own invocation is enough to distinguish
# "the gate ran this" from "the gate skipped it", exactly what a --with-bins tree looks like
# from aeon.sh's point of view: something executable at $SPIRA_LC_BIN/$SPIRA_WORK_BIN.
cat > "$TMP/stub-lc" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$TMP/stub-lc-invoked"
exit 0
STUB
chmod +x "$TMP/stub-lc"
cat > "$TMP/stub-work" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$TMP/stub-work-invoked"
exit 0
STUB
chmod +x "$TMP/stub-work"

# The shim IS the model: records whether bd was reachable and whether work-env.sh's
# restricted env (SPIRA_WORK_BEAD_ID) was present, then exits — this suite is about which
# environment aeon.sh hands the model, not about what the model does with it.
cat > "$BIN/claude" <<SHIM
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
if command -v bd >/dev/null 2>&1; then
    echo BD_FOUND > "$TMP/bd-found"
    bd -C "\${SPIRA_DB:-}" close "\$BEAD_ID" --reason "legacy path closed" >/dev/null 2>&1
fi
[ -n "\${SPIRA_WORK_BEAD_ID:-}" ] && echo RESTRICTED > "$TMP/restricted-env"
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}
run_aeon() {
    rm -rf "$SPIRA_RUN/worktree"
    "$HERE/aeon.sh" builder > "$TMP/out" 2>&1
    echo $?
}
bead_status() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status", ""))' 2>/dev/null
}
ledger_line() { grep " builder $1 " "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null | tail -1; }
fresh() { rm -f "$TMP/stub-lc-invoked" "$TMP/stub-work-invoked" "$TMP/bd-found" "$TMP/restricted-env"; testdb_reset; }

echo "test-lifecycle-enforce-gate.sh"

# ======================================================================================
# CASE A: binaries built and executable, lifecycle_enforce UNSET — the legacy path, not the
# fact that SPIRA_LC_BIN/SPIRA_WORK_BIN resolve to something executable, decides.
# ======================================================================================
echo
echo "case A: flag unset, binaries present — legacy path (the regression this bead fixes)"

export SPIRA_LC_BIN="$TMP/stub-lc" SPIRA_WORK_BIN="$TMP/stub-work"
unset SPIRA_LIFECYCLE_ENFORCE
fresh; seed sp-lea-1
rc="$(run_aeon)"
is    "aeon.sh exits 0"                                    "0"    "$rc"
is    "bead closed via the legacy path"                     "closed" "$(bead_status sp-lea-1)"
nowant "SEEN RED: the stub spira-lc was never invoked"      "" "$([ -e "$TMP/stub-lc-invoked" ] && echo INVOKED)"
nowant "SEEN RED: the stub work client was never invoked"   "" "$([ -e "$TMP/stub-work-invoked" ] && echo INVOKED)"
is    "bd was on the model's PATH (legacy, unwrapped)"      "BD_FOUND" "$(cat "$TMP/bd-found" 2>/dev/null || echo absent)"
is    "no work-env.sh restricted env reached the model"     "" "$(cat "$TMP/restricted-env" 2>/dev/null || true)"

# ======================================================================================
# CASE B: lifecycle_enforce=1 but neither binary exists — refuse loudly, never fall back.
# ======================================================================================
echo
echo "case B: flag set, binaries missing — refuses, never falls back to the legacy path"

export SPIRA_LIFECYCLE_ENFORCE=1
export SPIRA_LC_BIN="$TMP/nonexistent-lc" SPIRA_WORK_BIN="$TMP/nonexistent-work"
fresh; seed sp-lea-2
rc="$(run_aeon)"
is    "aeon.sh exits nonzero (a refusal, not a quiet idle)" "1" "$rc"
is    "the model was never launched"                        "" "$(cat "$TMP/bd-found" 2>/dev/null || true)"
is    "bead is back to open, not stuck claimed"              "open" "$(bead_status sp-lea-2)"
want  "ledger names the misconfiguration, not a generic failure" \
      "lifecycle-enforce-binary-missing" "$(ledger_line sp-lea-2)"

# ======================================================================================
# CASE C: lifecycle_enforce=1, only ONE binary missing — still refuses (both are required).
# ======================================================================================
echo
echo "case C: flag set, one binary present and one missing — still refuses"

export SPIRA_LC_BIN="$TMP/stub-lc" SPIRA_WORK_BIN="$TMP/nonexistent-work"
fresh; seed sp-lea-3
rc="$(run_aeon)"
is   "aeon.sh exits nonzero" "1" "$rc"
is   "bead is back to open" "open" "$(bead_status sp-lea-3)"
want "ledger names the misconfiguration" "lifecycle-enforce-binary-missing" "$(ledger_line sp-lea-3)"

tl_summary
