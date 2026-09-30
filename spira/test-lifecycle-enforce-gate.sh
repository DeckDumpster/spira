#!/usr/bin/env bash
# test-lifecycle-enforce-gate.sh — whether an aeon takes the lifecycle semantic layer is
#   lifecycle_enforce, an explicit config decision, never "work/spira-lc happen to exist".
#
# THE DEFECT THIS REPRODUCES (sp-74gzo). aeon.sh used to gate on the spira-lc and work
# binaries being executable alone, and conf.sh auto-resolved both from
# $SPIRA_REPO/target/release whenever unset. Any tree that has simply built the workspace —
# a --with-bins corpus run, a production checkout after `round.sh land` — flipped every aeon
# fixture onto the restricted path, whether or not the operator meant to cut over.
#
# SEEN RED FIRST: before the fix, case A below (binaries built, flag unset) takes the
# restricted path — the stub spira-lc/work are invoked and their marker files appear —
# because the old gate only asked "are these executable", and stub scripts are.
#
# The old cases B/C ("flag set, a binary missing — refuse") are deleted (sp-gypjk): tools are
# invoked by name on the launcher's PATH and a release always carries both, so there is no
# "binary missing" configuration left to refuse.
#
# tier: T1
# covers: aeon/src/* spira/conf.sh
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
printf '. "%s/lib.sh"\n' "$HERE" > "$SPIRA_HOME/lib.sh"   # the aeon binary sources <home>/lib.sh; this is the real one, as aeon.sh sourced it
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
command -v aeon >/dev/null 2>&1 || bail "aeon is not on PATH — refusing to run the real model"

# STAND-INS FOR "THE BINARIES ARE BUILT". This suite proves the GATE, not lc_claim_bead's own
# correctness (test-aeon-lifecycle-cutover.sh covers that end to end against a real spira-lc
# server) — an executable script that records its own invocation is enough to distinguish
# "the gate ran this" from "the gate skipped it", exactly what a --with-bins tree looks like
# from aeon's point of view: spira-lc and work found by name on PATH (the stub dir first).
STUBS="$TMP/stubs"; mkdir -p "$STUBS"
cat > "$STUBS/spira-lc" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$TMP/stub-lc-invoked"
exit 0
STUB
chmod +x "$STUBS/spira-lc"
cat > "$STUBS/work" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$TMP/stub-work-invoked"
exit 0
STUB
chmod +x "$STUBS/work"

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
    PATH="$STUBS:$PATH" aeon --home "$SPIRA_HOME" builder > "$TMP/out" 2>&1
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
# CASE A: spira-lc/work on PATH, lifecycle_enforce UNSET — the legacy path, not the fact
# that the tools are there, decides.
# ======================================================================================
echo
echo "case A: flag unset, binaries present — legacy path (the regression this bead fixes)"

unset SPIRA_LIFECYCLE_ENFORCE
fresh; seed sp-lea-1
rc="$(run_aeon)"
is    "aeon.sh exits 0"                                    "0"    "$rc"
is    "bead closed via the legacy path"                     "closed" "$(bead_status sp-lea-1)"
is    "SEEN RED: the stub spira-lc was never invoked"       "absent" "$([ -e "$TMP/stub-lc-invoked" ] && echo INVOKED || echo absent)"
is    "SEEN RED: the stub work client was never invoked"    "absent" "$([ -e "$TMP/stub-work-invoked" ] && echo INVOKED || echo absent)"
is    "bd was on the model's PATH (legacy, unwrapped)"      "BD_FOUND" "$(cat "$TMP/bd-found" 2>/dev/null || echo absent)"
is    "no work-env.sh restricted env reached the model"     "" "$(cat "$TMP/restricted-env" 2>/dev/null || true)"

tl_summary
