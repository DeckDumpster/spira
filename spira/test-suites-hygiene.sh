#!/usr/bin/env bash
#
# test-suites-hygiene.sh — quarantine hygiene: flake observations, reactivation, max-age.
#
# FOUR PROPERTIES VERIFIED.
#
# 1. FLAKE THRESHOLD (sp-n2ax8): two distinct runs inside the window file a report through
#    incident.sh; the suite is never written to suite-state and no branch is created — a red
#    suite is reported and attributed, never silently quarantined. One run does not report;
#    two runs whose timestamps fall outside the window do not either. Same run_id twice
#    counts as one (dedup).
#
# 2. REACTIVATION: a quarantined suite whose bead is LANDED and whose clean-run count
#    reaches SPIRA_QUARANTINE_CLEAN_RUNS is activated; either condition alone is not enough.
#
# 3. MAX AGE: a suite quarantined past SPIRA_QUARANTINE_MAX_AGE gets one operator mail
#    per quarantine period; a second hygiene pass does not send a second mail.
#
# POSITIVE CONTROL IN EVERY PART: the matcher is proved capable of finding an offender
# before its silence is believed (law-absence-needs-a-positive-control).
#
# REAL bd AGAINST A THROWAWAY DATABASE (law-prefer-the-real-dependency). Reactivation
# checks bead land_state via bd; a stub would hide real serialization or field-name
# divergences.
#
# NON-DEFAULT CONFIGURED VALUES: SPIRA_FLAKE_QUARANTINE_AT=2, SPIRA_FLAKE_WINDOW=3600,
# SPIRA_QUARANTINE_CLEAN_RUNS=3, SPIRA_QUARANTINE_MAX_AGE=86400. Asserting against
# shipped defaults passes just as well with the numbers written directly into the code,
# which is what the config keys exist to prevent.
#
# covers: spira/suites.sh spira/suite-state.sh spira/conf.sh spira/gate-check.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isz()    { [ "$2" -eq 0 ] && ok "$1" || bad "$1" "wanted 0 got $2"; }
isnz()   { [ "${2:-0}" -ne 0 ] && ok "$1" || bad "$1" "wanted nonzero got [$2]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-suites-hygiene.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-suites-hygiene
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up suites_hygiene || { echo "test-suites-hygiene: could not build fixture database"; exit 1; }

B() { "${SPIRA_BD:-bd}" -C "$SPIRA_DB" "$@"; }

# ---- FIXTURE LAYOUT ----
# SH/  — the fake "spira/" directory; suites.sh HERE derives repo as SH/..=TMP
# TMP/spira/ — same dir (SPIRA_SUITE_STATE_FILE default "spira/suite-state" appends to TMP)
#   suite-state — the lifecycle state file under test
# TMP/state/  — SPIRA_SUITES_STATE (flakeobs, clean-runs, maxage-mailed files)
# TMP/mail/   — SPIRA_MAIL mailboxes
# TMP/run/    — SPIRA_RUN
# TMP/bin/    — bd wrapper (preserves HOME for bd)
# TMP/home/   — fake HOME

SH="$TMP/spira"
STATE="$TMP/state"
MAIL="$TMP/mail"
RUND="$TMP/run"
BINDIR="$TMP/bin"
mkdir -p "$SH/chamber" "$STATE" "$RUND" "$BINDIR" "$TMP/home" "$TMP/run/events"

# Copy only the scripts suites.sh and its callees need.
for f in suites.sh lib.sh conf.sh suite-state.sh suite-covers.sh bead.sh mail.sh incident.sh incident-dedup-decision.py; do
    cp "$HERE/$f" "$SH/$f"
done
cp -r "$HERE/mail" "$SH/mail"
cp "$HERE/chamber/builder.fayth" "$SH/chamber/builder.fayth"

# Touch the suite-state file so suite_state_file finds a readable path.
touch "$SH/suite-state"
# Touch the suite files referenced by observe-flake so the existence check passes.
touch "$SH/test-hygiene-foo.sh"

# Init the fixture root as a git repo — a regression control for PART 1: crossing the flake
# threshold must never create a branch or touch this checkout (sp-n2ax8).
git init -q "$TMP"
git -C "$TMP" config user.email "flake-test@example.invalid"
git -C "$TMP" config user.name "Test"
git -C "$TMP" add -- spira/suite-state
git -C "$TMP" commit -q --no-gpg-sign -m "init"

# bd wrapper: bd needs the real HOME for dolt config; everything else is the fixture.
printf '#!/usr/bin/env bash\nHOME=%s exec %s "$@"\n' "$HOME" "$(type -P bd)" > "$BINDIR/bd"
chmod +x "$BINDIR/bd"

# sut: run suites.sh with a fully controlled environment.
# NON-DEFAULT values for all quarantine thresholds so we assert against config, not literals.
FLAKE_AT=2 FLAKE_WIN=3600 CLEAN_RUNS=3 MAX_AGE=86400
sut() {
    local _cmd="${1:-}"; shift
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_HOME="$SH" SPIRA_REPO="$TMP" SPIRA_HOME_REPO="fixture-repo" \
        SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BINDIR/bd" \
        SPIRA_RUN="$RUND" \
        SPIRA_SUITES_STATE="$STATE" \
        SPIRA_SUITE_STATE_FILE="spira/suite-state" \
        SPIRA_MAIL="$MAIL" SPIRA_MAIL_KINDS="$SH/mail/kinds" \
        SPIRA_FLAKE_QUARANTINE_AT="$FLAKE_AT" \
        SPIRA_FLAKE_WINDOW="$FLAKE_WIN" \
        SPIRA_QUARANTINE_CLEAN_RUNS="$CLEAN_RUNS" \
        SPIRA_QUARANTINE_MAX_AGE="$MAX_AGE" \
        SPIRA_PATH="$BINDIR" \
        bash "$SH/suites.sh" "$_cmd" "$@"
}

# Helpers to inspect the state file and per-suite files.
state_of()       { grep -E "^$1 \|" "$SH/suite-state" 2>/dev/null | awk -F'|' '{print $2}' | tr -d ' ' || echo active; }
bead_of_state()  { grep -E "^$1 \|" "$SH/suite-state" 2>/dev/null | awk -F'|' '{print $4}' | tr -d ' ' || echo ""; }
obs_count()      { wc -l < "$STATE/$1.flakeobs" 2>/dev/null || echo 0; }
mail_count_op()  { SPIRA_MAIL="$MAIL" SPIRA_MAIL_KINDS="$SH/mail/kinds" bash "$SH/mail.sh" count operator 2>/dev/null || echo 0; }
latest_mail_body_op() {
    local f; f="$(ls -t "$MAIL/operator/new"/* "$MAIL/operator/cur"/* 2>/dev/null | head -1)"
    [ -n "$f" ] && cat "$f"
}
# No branch is ever created by observe-flake (sp-n2ax8: the whole mechanism is gone).
any_branch_created() { git -C "$TMP" branch --list 'spira-suite-state/*' 2>/dev/null | head -1 | tr -d ' *'; }
bead_id_of_report() { printf '%s' "$1" | grep -oE 'bead: [a-z]+-[a-z0-9]+' | awk '{print $2}'; }
bead_title() {
    B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
    print(d.get("title") or "")
except Exception:
    pass
' 2>/dev/null
}

# Reset helpers.
reset_statefile() { : > "$SH/suite-state"; }
reset_state()     { rm -f "$STATE"/*.flakeobs "$STATE"/*.clean-runs "$STATE"/*.maxage-mailed; }
reset_mail()      { rm -rf "$MAIL"; }

# ======================================================================================
echo
echo "=== PART 1: flake observations ==="
# ======================================================================================

# -- positive control: non-existent suite is rejected --
echo
echo "SEEN RED: non-existent suite → rejected:"
reset_statefile; reset_state

out_ne="$(sut observe-flake no-such-suite.sh run-1 2>&1)"; rc_ne=$?
isnz "non-existent suite: non-zero exit" "$rc_ne"
want "non-existent suite: error names the suite" "no such suite" "$out_ne"

# -- positive control: two runs inside the window file a report, never quarantine --
echo
echo "two runs inside window → reported, never quarantined:"
reset_statefile; reset_state

# First call (run-1): count=1, below threshold=2, no report.
out1="$(sut observe-flake test-hygiene-foo.sh run-1 2>&1)"
st1="$(state_of test-hygiene-foo.sh)"
is "SEEN RED: first run: 1 in window, not reported yet" "active" "$st1"
nowant "first run: no report filed below threshold" "reported (bead:" "$out1"

# Second call: count=2 = threshold. Filed through incident.sh; suite-state and the checkout
# are never touched, and no branch is created.
out2="$(sut observe-flake test-hygiene-foo.sh run-2 2>&1)"
st2="$(state_of test-hygiene-foo.sh)"
is "second obs: suite-state never written (stays active)" "active" "$st2"
nobranch="$(any_branch_created)"
is "second obs reaches threshold: no branch created" "" "$nobranch"
want "flake report filed through incident.sh" "reported (bead:" "$out2"
bid_reported="$(bead_id_of_report "$out2")"
isnz "flake report: bead id captured" "${#bid_reported}"
if [ -n "${bid_reported:-}" ]; then
    want "flake report bead is titled as a question, not a quarantine notice" \
        "why does test-hygiene-foo.sh fail intermittently" "$(bead_title "$bid_reported")"
fi

# -- same run_id twice: dedup fires, only one observation recorded --
echo
echo "same run_id twice → one observation, not quarantined:"
reset_statefile; reset_state

sut observe-flake test-hygiene-foo.sh run-dedup >/dev/null 2>&1
sut observe-flake test-hygiene-foo.sh run-dedup >/dev/null 2>&1
st_dedup="$(state_of test-hygiene-foo.sh)"
obs_dedup="$(obs_count test-hygiene-foo.sh)"
is "same run twice: suite stays active (dedup)"     "active" "$st_dedup"
is "same run twice: exactly one line in ledger"     "1"      "$obs_dedup"

# -- one observation: stays active --
echo
echo "one observation → not quarantined:"
reset_statefile; reset_state

sut observe-flake test-hygiene-foo.sh run-1 >/dev/null 2>&1
st3="$(state_of test-hygiene-foo.sh)"
is "one obs below threshold: suite stays active" "active" "$st3"

# -- two observations outside the window: does not quarantine --
echo
echo "two obs outside window → not quarantined:"
reset_statefile; reset_state

# Plant two old observations (outside FLAKE_WIN=3600s) directly in the file.
old_epoch=$(( $(date +%s) - FLAKE_WIN - 100 ))
printf '%s run-old\n%s run-old\n' "$old_epoch" "$old_epoch" > "$STATE/test-hygiene-foo.sh.flakeobs"

# One new observation (distinct run) adds count-in-window=1; still below threshold.
sut observe-flake test-hygiene-foo.sh run-new >/dev/null 2>&1
st4="$(state_of test-hygiene-foo.sh)"
is "two old obs + one new = 1 in window: not quarantined" "active" "$st4"

# ======================================================================================
echo
echo "=== PART 2: reactivation ==="
# ======================================================================================

# Create a bead in the testdb and mark it LANDED.
reset_statefile; reset_state

bead_id="$(B create "test-hygiene: fix test-hygiene-bar.sh" -l "plan,repo:fixture-repo" 2>/dev/null \
    | grep -oE '[a-z]+-[a-z0-9]+' | head -1)" || bead_id=""
[ -n "${bead_id:-}" ] || { bad "reactivation setup: could not create bead" "bd create failed"; }
[ -n "${bead_id:-}" ] || { echo "  SKIP: reactivation tests (no bead id)"; }

if [ -n "${bead_id:-}" ]; then
    B set-state "$bead_id" land_state=LANDED --reason "test-landed" >/dev/null 2>&1 || true
    land_state="$(B show "$bead_id" --json 2>/dev/null | python3 -c '
import json,sys
try:
    d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d
    lbls = d.get("labels") or []
    print("LANDED" if "land_state:LANDED" in lbls else "")
except Exception: pass
' 2>/dev/null)" || land_state=""
    is "reactivation setup: bead has land_state:LANDED" "LANDED" "${land_state:-NOT_LANDED}"

    # Write the suite as quarantined with this bead, since=now.
    since="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '%s | quarantined | %s | %s | test reason\n' \
        "test-hygiene-bar.sh" "$since" "$bead_id" >> "$SH/suite-state"

    # -- positive control: bead LANDED + N clean runs → reactivated --
    echo
    echo "SEEN RED: bead LANDED + N clean runs → reactivated:"
    # Plant N clean runs.
    printf '%d\n' "$CLEAN_RUNS" > "$STATE/test-hygiene-bar.sh.clean-runs"

    out_hyg="$(sut hygiene 2>&1)"
    st5="$(state_of test-hygiene-bar.sh)"
    is "bead LANDED + $CLEAN_RUNS clean runs: reactivated" "active" "$st5"
    want "hygiene reports reactivation" "reactivated" "$out_hyg"
    react_body="$(latest_mail_body_op)"
    want "reactivation mail's rendered block leads with the bead's id and title" \
        "$bead_id: test-hygiene: fix test-hygiene-bar.sh" "$react_body"

    # -- bead not LANDED + N clean runs → not reactivated --
    echo
    echo "bead NOT LANDED + N clean runs → not reactivated:"
    reset_statefile; reset_state

    bead2_id="$(B create "test-hygiene: open bead" -l "plan,repo:fixture-repo" 2>/dev/null \
        | grep -oE '[a-z]+-[a-z0-9]+' | head -1)" || bead2_id=""
    if [ -n "${bead2_id:-}" ]; then
        since="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf '%s | quarantined | %s | %s | test reason\n' \
            "test-hygiene-bar.sh" "$since" "$bead2_id" >> "$SH/suite-state"
        printf '%d\n' "$CLEAN_RUNS" > "$STATE/test-hygiene-bar.sh.clean-runs"
        sut hygiene >/dev/null 2>&1
        st6="$(state_of test-hygiene-bar.sh)"
        is "bead not LANDED + $CLEAN_RUNS clean runs: stays quarantined" "quarantined" "$st6"
    fi

    # -- bead LANDED + N-1 clean runs → not reactivated --
    echo
    echo "bead LANDED + N-1 clean runs → not reactivated:"
    reset_statefile; reset_state

    since="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '%s | quarantined | %s | %s | test reason\n' \
        "test-hygiene-bar.sh" "$since" "$bead_id" >> "$SH/suite-state"
    printf '%d\n' $(( CLEAN_RUNS - 1 )) > "$STATE/test-hygiene-bar.sh.clean-runs"
    sut hygiene >/dev/null 2>&1
    st7="$(state_of test-hygiene-bar.sh)"
    is "bead LANDED + $(( CLEAN_RUNS - 1 )) clean runs: stays quarantined" "quarantined" "$st7"
fi

# ======================================================================================
echo
echo "=== PART 3: max-age mail ==="
# ======================================================================================

echo
echo "SEEN RED: max-age exceeded → operator mailed once:"
reset_statefile; reset_state; reset_mail

# Plant a quarantine with since = now - MAX_AGE - 60 (clearly past threshold).
old_since="$(date -u -d "@$(( $(date +%s) - MAX_AGE - 60 ))" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)"
if [ -n "${old_since:-}" ]; then
    printf '%s | quarantined | %s | %s | test reason\n' \
        "test-hygiene-baz.sh" "$old_since" "${bead_id:-sp-fake}" >> "$SH/suite-state"

    # First hygiene pass: should mail the operator.
    sut hygiene >/dev/null 2>&1
    m1="$(mail_count_op)"
    isnz "max-age exceeded: operator receives mail" "$m1"
    if [ -n "${bead_id:-}" ]; then
        maxage_body="$(latest_mail_body_op)"
        want "max-age mail's rendered block leads with the bead's id and title" \
            "$bead_id: test-hygiene: fix test-hygiene-bar.sh" "$maxage_body"
    fi

    # Second hygiene pass: must NOT send another mail.
    sut hygiene >/dev/null 2>&1
    m2="$(mail_count_op)"
    is "second hygiene pass: no second mail" "$m1" "$m2"
else
    bad "max-age test: date -d is unavailable on this platform" "skip"
fi

printf '\nASSERTIONS %d\n' $(( pass + fail ))
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
