#!/usr/bin/env bash
#
# test-groom-escalation-check.sh — groom_claims_verified: a groom log line claiming
# ESCALATED without a matching ask bead is unproven; one with the ask is not.
# FAYTH_GROOM_ESCALATION_CHECK tells aeon.sh to reopen and poison the trigger on the
# unproven case.
#
#   ./test-groom-escalation-check.sh
#
# WHAT THIS SUITE IS GUARDING
# ---------------------------
# The groomer has a defect: it logs "ESCALATED sp-X" without ever calling mail, and
# the sentinel accepts the log line as evidence (CHECK5 only checks the line exists).
# FAYTH_GROOM_ESCALATION_CHECK=1 tells aeon.sh to verify the claim against the database:
# if no ask bead (type=decision, needs-operator label) was created in this session naming
# that bead, the trigger is reopened and poisoned.
#
# THE FAYTH IS NAMED SOMETHING ELSE, on purpose — same rationale as test-ops-closing.sh.
# The check is declared by FAYTH_GROOM_ESCALATION_CHECK, not by the name "groomer". A
# fayth called `scrubber` that sets the key is held to the rule; one that does not is not.
# Asserting through groomer.fayth directly would pass against a check keyed on the name.
#
# RETIRED (wave 4.34, sp-27d3d): groom_claims_verified was lib.sh; it is ported to
# aeon::trace::groom_claims_verified, called in-process from Run::verdict
# (aeon/src/verdict.rs). The T1 table that drove it through a bash sourcing shim (no
# escalation keyword, a claim with no ask bead, a fresh matching ask, an ask bead created
# before the session epoch, the id named in the description rather than the title, two
# claims with only one backed, "flagged" as a claim keyword, and an empty log) moved to
# groom_claims_verified_table (aeon/src/trace.rs). T3 (below) is unaffected — it drives the
# real `aeon` binary end to end and does not care whether the family lives in bash or Rust.
#
# tier: T3
# covers: aeon/src/* spira/chamber/groomer.fayth spira/conf.sh UC-aeon-execution-16
# defect: sp-yr4ih
# scar: groomer wrote ESCALATED to the groom log without calling mail; sentinel accepted the log line as evidence of the escalation
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# The tree's tools for a minimal-PATH run (sp-gypjk): where this suite's PATH finds the
# tree's build, and the tree's own spira/.
TOOLS="$(dirname "$(command -v spira-config)"):$HERE"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-groom-escalation-check.sh"

# ===========================================================================================
echo
echo "T3: real aeon runs prove the wiring"
# ===========================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-groom-escalation-check
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up groom_esc || { echo "test-groom-escalation-check: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

HOMEDIR="$TMP/home"; mkdir -p "$HOMEDIR/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$HOMEDIR/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$HOMEDIR/"
cp -r "$HERE/actors" "$HOMEDIR/" 2>/dev/null || true
RUN="$TMP/run"; mkdir -p "$RUN"
GROOM_LOG="$RUN/groom.log"
REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$REPO_MAP"

# THE GROOM PARTITION LABEL. Use a non-default so the test proves the fayth predicate
# reads the key rather than a literal. A test asserting against the default "groom" would
# pass even if the code hardcoded it (law-gates-run-in-a-clean-environment).
GROOM_LABEL="test-hygiene"

# TWO FAYTHS. `scrubber` declares the check; `tiler` does not.
# This proves the check is bound to the declaration rather than to the name.
for f in scrubber tiler; do
    cat > "$HOMEDIR/chamber/$f.fayth" <<FAYTH
FAYTH_NAME=$f
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}$GROOM_LABEL"
FAYTH_EXCLUDE_LABELS="spira-poison,\${SPIRA_ASK_LABEL}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
    printf 'trigger {{BEAD_ID}} scrub\n' > "$HOMEDIR/chamber/$f.md"
done
printf 'FAYTH_GROOM_ESCALATION_CHECK=1\n' >> "$HOMEDIR/chamber/scrubber.fayth"

# THE SHIM IS THE SESSION. It always closes the trigger bead and optionally writes the
# groom log and/or files an ask bead, controlled by $TMP/act.
# The guard against the real model: conf.sh REPLACES $PATH, so shimming by PATH alone
# would invoke the real model.
BIN="$TMP/bin"; mkdir -p "$BIN"
# aeon and the spira-claim it ranks through are found by name on the suite's PATH, which
# run_aeon's env -i carries over (sp-gypjk).
command -v aeon >/dev/null 2>&1 \
    || { echo "test-groom-escalation-check: aeon is not on PATH — refusing to run the real model" >&2; exit 1; }
command -v spira-claim >/dev/null 2>&1 \
    || { echo "test-groom-escalation-check: spira-claim is not on PATH — the aeon cannot claim" >&2; exit 1; }
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^trigger \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
# Commit something so aeon.sh sees committed=yes and the groom check is the only
# verdict mechanism that fires. Without a commit aeon.sh reopens for a different
# reason before the groom check can run.
printf 'groom pass for %s\n' "$id" >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — groom pass"
case "$(cat "$TMP/act")" in
    log-claim)
        # Write ESCALATED to the groom log without filing an ask bead.
        printf '%s groom: pass complete. Examined 1 beads. LIVELOCK rows: 0. Actions: ESCALATED sp-gc1.\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$GROOM_LOG"
        ;;
    log-and-ask)
        # Write ESCALATED and file a matching ask bead.
        bd -C "$SPIRA_DB" create "Close sp-gc2 as litter?" \
            -l "needs-operator,overseer" --type decision --silent >/dev/null 2>&1
        printf '%s groom: pass complete. Examined 1 beads. LIVELOCK rows: 0. Actions: ESCALATED sp-gc2.\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$GROOM_LOG"
        ;;
    old-ask)
        # Ask bead exists but was created BEFORE the session — must not excuse the claim.
        printf '%s groom: pass complete. Examined 1 beads. LIVELOCK rows: 0. Actions: ESCALATED sp-gc3.\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$GROOM_LOG"
        ;;
    no-claim)
        # Log line with no escalation keyword — no claim, so no check.
        printf '%s groom: pass complete. Examined 2 beads. LIVELOCK rows: 0. Actions: CLOSED sp-xx premise-gone.\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$GROOM_LOG"
        ;;
esac
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

run_aeon() {    # run_aeon <fayth> <act>
    printf '%s' "$2" > "$TMP/act"
    rm -rf "$RUN/worktree"
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" TMP="$TMP" \
        GROOM_LOG="$GROOM_LOG" \
        SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_HOME="$HOMEDIR" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$REPO_MAP" SPIRA_AGENT="$BIN/claude" \
        SPIRA_SCOPE_LABEL="${SPIRA_SCOPE_LABEL:-}" \
        BEADS_NO_AUTO_IMPORT=1 \
        timeout 240 aeon --home "$HOMEDIR" "$1" > "$TMP/out" 2>&1
}
field()  { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }
labels() { bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | tr '\n' ' '; }

# seed a trigger bead for the scrubber/tiler lane.
# repo:fixture matches the repo map entry so aeon.sh resolves the workspace.
# delivers:note:$GROOM_LOG mirrors the real groom-trigger.sh (and maechen-trigger.sh),
# which files exactly this label on a task bead that closes on a log, never a commit —
# without it a work-type close with nothing committed goes through the submitted
# conversion instead of the legacy commit/delivers audit this suite means to exercise
# (sp-qsona: delivers: exempts a work bead from the submitted pipeline).
seed_trigger() {  # seed_trigger <id>
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"$GROOM_LABEL\",\"repo:fixture\",\"delivers:note:$GROOM_LOG\""
    printf '{"id":"%s","title":"Groomer pass","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-20T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}

fresh() {    # fresh <id>
    testdb_reset
    : > "$GROOM_LOG"
    seed_trigger "$1"
}

echo
echo "a groom log that claims ESCALATED without an ask bead — trigger is poisoned:"
fresh sp-gc-1; run_aeon scrubber log-claim
is   "trigger is reopened"   open        "$(field sp-gc-1 status)"
want "trigger is poisoned"   "spira-poison" "$(labels sp-gc-1)"
want "log says REOPENED and POISONED" "REOPENED and POISONED" "$(cat "$TMP/out")"
want "poison message names the uncorroborated bead" "sp-gc1" "$(cat "$TMP/out")"

echo
echo "a groom log that claims ESCALATED WITH a matching ask bead — trigger is NOT poisoned:"
fresh sp-gc-2; run_aeon scrubber log-and-ask
is     "trigger stays closed"     closed        "$(field sp-gc-2 status)"
nowant "trigger is not poisoned"  "spira-poison" "$(labels sp-gc-2)"
want   "check saw the ask bead"   "groom-escalation-check: all claimed escalations verified" "$(cat "$TMP/out")"

echo
echo "a TILER (no FAYTH_GROOM_ESCALATION_CHECK) that claims ESCALATED — not held to the rule:"
fresh sp-gc-5; run_aeon tiler log-claim
is     "trigger stays closed"     closed        "$(field sp-gc-5 status)"
nowant "trigger is not poisoned"  "spira-poison" "$(labels sp-gc-5)"
nowant "check did not run at all" "groom-escalation-check" "$(cat "$TMP/out")"

echo
echo "SPIRA_GROOM_ASK_LABEL is in the conf key list and defaults to groom-asked:"
keys="$(env -i HOME="$TMP" PATH="$TOOLS:/usr/bin:/bin" SPIRA_CONF="$TMP/none.conf" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_CONF_KEYS"' 2>/dev/null)"
want "SPIRA_GROOM_ASK_LABEL is in the key list" "SPIRA_GROOM_ASK_LABEL" "$keys"

val="$(env -i HOME="$TMP" PATH="$TOOLS:/usr/bin:/bin" SPIRA_CONF="$TMP/none.conf" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_GROOM_ASK_LABEL"' 2>/dev/null)"
is "SPIRA_GROOM_ASK_LABEL defaults to groom-asked" "groom-asked" "$val"

tl_summary
