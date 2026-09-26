#!/usr/bin/env bash
#
# test-groom-escalation-check.sh — groom_claims_verified (lib.sh): a groom log line
# claiming ESCALATED without a matching ask bead is unproven; one with the ask is not.
# FAYTH_GROOM_ESCALATION_CHECK tells aeon.sh to reopen and poison the trigger on the
# unproven case.
#
#   ./test-groom-escalation-check.sh
#
# WHAT THIS SUITE IS GUARDING
# ---------------------------
# The groomer has a defect: it logs "ESCALATED sp-X" without ever calling mail.sh, and
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
# covers: spira/aeon.sh spira/lib.sh spira/chamber/groomer.fayth spira/conf.sh
# defect: sp-yr4ih
# scar: groomer wrote ESCALATED to the groom log without calling mail.sh; sentinel accepted the log line as evidence of the escalation
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-groom-escalation-check.sh"

gcv() {   # gcv <log> <ask-json> <epoch> -> groom_claims_verified's own output
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; groom_claims_verified "$2" "$3" "$4"' \
        _ "$HERE" "$1" "$2" "$3" 2>/dev/null
}

# ===========================================================================================
echo
echo "T1: groom_claims_verified <new-log-lines> <ask-json> <epoch> — no aeon run, no bd"
# ===========================================================================================
EPOCH=1900000000
NEW_TS="$(date -u -d "@$((EPOCH + 60))" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || printf '2030-03-11T05:01:00Z')"
OLD_TS="$(date -u -d "@$((EPOCH - 3600))" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || printf '2030-03-11T04:00:00Z')"

LOG_CLAIM='2026-09-20T00:00:00Z groom: pass complete. Actions: ESCALATED sp-gc1.'
LOG_NOCLAIM='2026-09-20T00:00:00Z groom: pass complete. Actions: CLOSED sp-xx premise-gone.'
LOG_MULTI='2026-09-20T00:00:00Z groom: Actions: ESCALATED sp-gc1. Also flagged sp-gc2 for review.'

ASK_NONE='[]'
ask_for() {   # ask_for <id> <created_at> [field: title|description]
    python3 -c 'import json,sys; print(json.dumps([{"title": (sys.argv[1] if sys.argv[3]!="description" else "unrelated"), "description": (sys.argv[1] if sys.argv[3]=="description" else ""), "created_at": sys.argv[2]}]))' \
        "Close $1 as litter?" "$2" "${3:-title}"
}

out="$(gcv "$LOG_NOCLAIM" "$ASK_NONE" "$EPOCH")"
is "no escalation keyword at all: nothing claimed, nothing unproven" "" "$out"

out="$(gcv "$LOG_CLAIM" "$ASK_NONE" "$EPOCH")"
is "a claim with no ask bead at all is unproven" "sp-gc1" "$out"

out="$(gcv "$LOG_CLAIM" "$(ask_for sp-gc1 "$NEW_TS")" "$EPOCH")"
is "a claim backed by a matching, fresh ask bead is verified" "" "$out"

out="$(gcv "$LOG_CLAIM" "$(ask_for sp-gc1 "$OLD_TS")" "$EPOCH")"
is "an ask bead created BEFORE the session epoch does not excuse the claim" "sp-gc1" "$out"

out="$(gcv "$LOG_CLAIM" "$(ask_for sp-gc1 "$NEW_TS" description)" "$EPOCH")"
is "the id may be named in the description rather than the title" "" "$out"

out="$(gcv "$LOG_MULTI" "$(ask_for sp-gc1 "$NEW_TS")" "$EPOCH")"
is "two claims, only one backed: the other is named unproven" "sp-gc2" "$out"

out="$(gcv "$LOG_MULTI" "$(python3 -c 'import json; print(json.dumps([]))')" "$EPOCH")"
want "'flagged' is recognised as a claim keyword, same as ESCALATED" "sp-gc2" "$out"

out="$(gcv "" "$ASK_NONE" "$EPOCH")"
is "an empty log: nothing claimed, nothing unproven" "" "$out"

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
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$HERE/suite-covers.sh" "$HOMEDIR/"
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
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-groom-escalation-check: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }
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
        timeout 240 bash "$HOMEDIR/aeon.sh" "$1" > "$TMP/out" 2>&1
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
keys="$(env -i HOME="$TMP" PATH="/usr/bin:/bin" SPIRA_CONF="$TMP/none.conf" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_CONF_KEYS"' 2>/dev/null)"
want "SPIRA_GROOM_ASK_LABEL is in the key list" "SPIRA_GROOM_ASK_LABEL" "$keys"

val="$(env -i HOME="$TMP" PATH="/usr/bin:/bin" SPIRA_CONF="$TMP/none.conf" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_GROOM_ASK_LABEL"' 2>/dev/null)"
is "SPIRA_GROOM_ASK_LABEL defaults to groom-asked" "groom-asked" "$val"

tl_summary
