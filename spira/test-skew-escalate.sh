#!/usr/bin/env bash
#
# test-skew-escalate.sh — escalate() outputs failures to stdout; a silent failure is
# indistinguishable from a quiet-because-nothing-happened pass.
#
#   ./test-skew-escalate.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# escalate() in skew was swallowing both streams of the mail call
# (>/dev/null 2>&1), so a failing send produced no output in skew.log and the
# divergence went unreported for an entire day while the timer ran every hour.
# This suite proves the fix: a send the mail library refuses (in-process since sp-ipwlic)
# produces output on stdout and returns non-zero.
#
# THE POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control). Before
# claiming the check catches escalation failure, prove the escalation path is
# reached at all: a successful notify must produce output on stdout. A check that
# always claims "escalation failed" would pass every subsequent assertion without
# proving the path was exercised.
#
# THE FIXTURE IS A REAL GIT REPO with release tags, and a releases directory where
# the activated release is not the latest, so check() reaches the NOT-LATEST path
# and calls escalate(). No shared state with the installed harness is used
# (law-gates-run-in-a-clean-environment).
#
# tier: T1
# covers: skew/src/* UC-operator-channel-25
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-skew-escalate.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture: a git repo with two release tags so check() finds NOT-LATEST and
# calls escalate() when the older release is activated.
# ---------------------------------------------------------------------------
REPO="$TMP/repo"
git init -q "$REPO"
git -C "$REPO" config user.email "test@test"
git -C "$REPO" config user.name "test"
# SPIRA_HOME_REPO/SPIRA_REPO_MAP EXPLICITLY: home_repo() defaults to the fixture's own
# "spira" and repo_root("spira") then has no map to resolve it against, so skew/src's
# landref fails with "cannot resolve the ref repo:spira lands on" before check() ever
# reaches escalate(). A repo's base is no longer derived (no remote-HEAD/current-branch
# fallback to rely on) — the map row must name it explicitly, so this reads the checkout's
# actual initial branch name rather than assuming "main"/"master".
BR="$(git -C "$REPO" symbolic-ref --short HEAD)"
SKEWMAP="$TMP/repomap"
printf 'fixture | %s | push | %s | | true | self\n' "$REPO" "$BR" > "$SKEWMAP"
mkdir -p "$REPO/spira"
printf '# boundary\n'        > "$REPO/spira/boundary"
printf '#!/usr/bin/env bash\n' > "$REPO/spira/gate.sh"
printf '#!/usr/bin/env bash\n' > "$REPO/spira/lib.sh"
git -C "$REPO" add spira/
git -C "$REPO" commit -q -m "base"
COMMIT1="$(git -C "$REPO" rev-parse HEAD)"

printf '# v2\n' >> "$REPO/spira/lib.sh"
git -C "$REPO" add spira/lib.sh
git -C "$REPO" commit -q -m "advance"
COMMIT2="$(git -C "$REPO" rev-parse HEAD)"

TS1="20260912T100000Z"
TS2="20260912T120000Z"
git -C "$REPO" tag -a "spira-release-spira-${TS1}" "$COMMIT1" -m "release v1"
git -C "$REPO" tag -a "spira-release-spira-${TS2}" "$COMMIT2" -m "release v2"

RELEASES="$TMP/releases"
mkdir -p "$RELEASES/spira-${TS1}" "$RELEASES/spira-${TS2}"
printf 'commit %s\ntimestamp %s\n' "$COMMIT1" "$TS1" > "$RELEASES/spira-${TS1}/MANIFEST"
printf 'commit %s\ntimestamp %s\n' "$COMMIT2" "$TS2" > "$RELEASES/spira-${TS2}/MANIFEST"
# Activate the older release so check() finds NOT-LATEST and calls escalate().
ln -s "spira-${TS1}" "$RELEASES/current"

# A skew runner with an explicit minimal environment. Each call uses its own
# SPIRA_RUN directory so the dedupe stamp never suppresses a second escalation
# in the same test run. HOME is required by conf.sh for the SPIRA_DB default.
#
# run_skew <env-var=val>...        — check --escalate (escalation mode)
# run_skew_ro <env-var=val>...     — check alone (read-only mode; must not call SPIRA_NOTIFY)
# run_skew_shared <run_dir> <env-var=val>... — check --escalate reusing <run_dir> (dedupe test)
#
# Every call site below sets SPIRA_HOME="$HERE" (this checkout's real spira/, with a real
# lib.sh), not the mail-stub-only *_HOME directory that goes first on PATH. `skew` (a
# compiled binary, sp-yyk47) resolves lib.sh through SPIRA_HOME, unlike skew.sh, which found
# it beside its OWN script regardless of SPIRA_HOME's value — so *_HOME (which holds only a
# stub mail) was always safe to name there for bash, but would make `skew` exit 3 before
# it ever reached the escalate() path this suite exists to test.
run_skew() {
    local run_dir
    run_dir="$(mktemp -d "$TMP/run-XXXXX")"
    tl_config SPIRA_RUN="$run_dir" SPIRA_DOLT_DATA="" SPIRA_RELEASES="$RELEASES" \
        SPIRA_HOME_REPO="fixture" SPIRA_REPO_MAP="$SKEWMAP"
    env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_REPO="$REPO" \
        "${@}" \
        skew check --escalate 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

run_skew_ro() {
    local run_dir
    run_dir="$(mktemp -d "$TMP/run-XXXXX")"
    tl_config SPIRA_RUN="$run_dir" SPIRA_DOLT_DATA="" SPIRA_RELEASES="$RELEASES" \
        SPIRA_HOME_REPO="fixture" SPIRA_REPO_MAP="$SKEWMAP"
    env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_REPO="$REPO" \
        "${@}" \
        skew check 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

run_skew_shared() {
    local run_dir="$1"; shift
    tl_config SPIRA_RUN="$run_dir" SPIRA_DOLT_DATA="" SPIRA_RELEASES="$RELEASES" \
        SPIRA_HOME_REPO="fixture" SPIRA_REPO_MAP="$SKEWMAP"
    env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_REPO="$REPO" \
        "${@}" \
        skew check --escalate 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# ===========================================================================
echo
echo "positive control — successful notify appears on stdout:"
# ===========================================================================
# THE MAIL IS THE MAIL LIBRARY, IN-PROCESS (sp-ipwlic): skew no longer execs a `mail`
# program a stub on PATH could stand in for, so each section points the real send at a
# mailbox tree of its own (mail is registered config: SPIRA_MAIL and its kinds, index and mute)
# and reads what was delivered there.
mail_at() {  # mail_at <root> [<kinds dir>] — this suite's mail config for the next skew runs
    mkdir -p "$1"
    tl_config SPIRA_MAIL="$1" SPIRA_MAIL_KINDS="${2:-$HERE/mail/kinds}" \
        SPIRA_MAIL_INDEX="$1/index" SPIRA_MAIL_MUTE=0
}
delivered() { find "$1" -path '*/new/*' -type f 2>/dev/null | wc -l | tr -d ' '; }

GOOD_MAIL="$TMP/good-mail"
mail_at "$GOOD_MAIL"
good_out="$(run_skew SPIRA_HOME="$HERE")"; good_rc=$?
# NOT-LATEST was found; exit 1 is correct.
is  "positive control exits 1 (divergence found)" "1" "$good_rc"
# The escalation confirmation must appear on stdout so skew.log has it.
want "positive control: escalation noted on stdout" "escalated" "$good_out"
is  "positive control: exactly one message delivered" "1" "$(delivered "$GOOD_MAIL")"
GOOD_BODY="$(find "$GOOD_MAIL" -path '*/new/*' -type f 2>/dev/null | head -1)"
# Body must be non-empty — an empty body is indistinguishable from a correct one
# by exit code alone (law-absence-needs-a-positive-control).
[ -n "$GOOD_BODY" ] && [ -s "$GOOD_BODY" ] && ok "body is non-empty" \
    || bad "body is non-empty" "no delivered message under $GOOD_MAIL"
want "body contains ## Question" "## Question" "$(cat "$GOOD_BODY" 2>/dev/null)"
want "body contains ## Default"  "## Default"  "$(cat "$GOOD_BODY" 2>/dev/null)"

# ===========================================================================
echo
echo "failing notify — failure message appears on stdout, returns non-zero:"
# ===========================================================================
# A send the mail library refuses: no kind is registered, so `question` fails mail's own lint.
BAD_MAIL="$TMP/bad-mail"; mkdir -p "$TMP/no-kinds"
mail_at "$BAD_MAIL" "$TMP/no-kinds"
fail_out="$(run_skew SPIRA_HOME="$HERE")"; fail_rc=$?
is  "failing notify exits 1" "1" "$fail_rc"
want "failing notify message on stdout"  "escalation failed" "$fail_out"
want "failing notify: the send's own refusal is included" "unknown kind question" "$fail_out"
is  "failing notify: nothing was delivered" "0" "$(delivered "$BAD_MAIL")"

# ===========================================================================
echo
echo "read-only mode — check without --escalate must not call mail:"
# ===========================================================================
RO_MAIL="$TMP/ro-mail"
mail_at "$RO_MAIL"
ro_out="$(run_skew_ro SPIRA_HOME="$HERE")"; ro_rc=$?
is  "read-only exits 1 (divergence found)" "1" "$ro_rc"
is  "read-only: mail not sent" "0" "$(delivered "$RO_MAIL")"
want   "read-only: NOT-LATEST finding still printed" "NOT-LATEST" "$ro_out"
nowant "read-only: no 'escalated' line"              "escalated"  "$ro_out"
nowant "read-only: no 'mail not found' line"      "mail not found" "$ro_out"

# ===========================================================================
echo
echo "condition-keyed dedupe — same condition, same run dir, not re-escalated:"
# ===========================================================================
DEDUPE_MAIL="$TMP/dedupe-mail"
mail_at "$DEDUPE_MAIL"
DEDUPE_RUN="$(mktemp -d "$TMP/dedup-run-XXXXX")"

first_out="$(run_skew_shared "$DEDUPE_RUN" SPIRA_HOME="$HERE")"; first_rc=$?
is  "dedupe first call exits 1 (divergence)" "1" "$first_rc"
want "dedupe first call: escalated" "escalated" "$first_out"
is  "dedupe first call: one message delivered" "1" "$(delivered "$DEDUPE_MAIL")"

second_out="$(run_skew_shared "$DEDUPE_RUN" SPIRA_HOME="$HERE")"; second_rc=$?
is  "dedupe second call exits 1 (divergence still present)" "1" "$second_rc"
nowant "dedupe second call: no re-escalation" "escalated" "$second_out"
is  "dedupe second call: still exactly one message delivered in total" "1" "$(delivered "$DEDUPE_MAIL")"

# ===========================================================================
echo
tl_summary
