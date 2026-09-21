#!/usr/bin/env bash
#
# test-groom-escalation-check.sh — FAYTH_GROOM_ESCALATION_CHECK: a groom log line
# claiming ESCALATED without a matching ask bead poisons the trigger; one with the ask
# does not.
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
# THE PAIR (law-absence-needs-a-positive-control)
# A silent claim is required to poison. A claim backed by a real ask bead is required NOT
# to. Both sides prove the check is neither always-on nor always-off.
#
# THE FAYTH IS NAMED SOMETHING ELSE, on purpose — same rationale as test-ops-closing.sh.
# The check is declared by FAYTH_GROOM_ESCALATION_CHECK, not by the name "groomer". A
# fayth called `scrubber` that sets the key is held to the rule; one that does not is not.
# Asserting through groomer.fayth directly would pass against a check keyed on the name.
#
# Driven through the REAL aeon.sh and the REAL bd on a throwaway fixture. The check is a
# comparison of what the log says against what the database contains, and a stub of either
# side would be a second implementation of the thing under test
# (law-prefer-the-real-dependency).
#
# covers: spira/aeon.sh spira/chamber/groomer.fayth spira/conf.sh
# defect: sp-yr4ih
# scar: groomer wrote ESCALATED to the groom log without calling mail.sh; sentinel accepted the log line as evidence of the escalation
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-groom-escalation-check.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-groom-escalation-check
TMP="$(mktemp -d)"
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
seed_trigger() {  # seed_trigger <id>
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"$GROOM_LABEL\",\"repo:fixture\""
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
echo "an OLD ask bead (before session epoch) does NOT excuse the claim — trigger is poisoned:"
# Seed the ask bead with a timestamp from an hour ago so its created_at is unambiguously
# before SESSION_EPOCH regardless of clock granularity. bd create would set created_at=now
# and might match SESSION_EPOCH within the same second; seeding with a fixed past timestamp
# removes that race (law-fixtures-carry-real-cadence).
fresh sp-gc-3
_old_ts="$(date -u -d '1 hour ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || printf '2026-09-20T00:00:00Z')"
printf '{"id":"sp-gc3-ask","title":"Close sp-gc3?","status":"open","issue_type":"decision","labels":["needs-operator","overseer"],"created_at":"%s","updated_at":"%s"}\n' \
    "$_old_ts" "$_old_ts" | testdb_seed
printf 'planted old ask bead for sp-gc3 (created_at=%s)\n' "$_old_ts"
run_aeon scrubber old-ask
is   "trigger is reopened"   open        "$(field sp-gc-3 status)"
want "trigger is poisoned"   "spira-poison" "$(labels sp-gc-3)"

echo
echo "a groom log with no escalation keyword — trigger closes clean:"
fresh sp-gc-4; run_aeon scrubber no-claim
is     "trigger stays closed"     closed        "$(field sp-gc-4 status)"
nowant "trigger is not poisoned"  "spira-poison" "$(labels sp-gc-4)"

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

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
