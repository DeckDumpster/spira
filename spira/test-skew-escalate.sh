#!/usr/bin/env bash
#
# test-skew-escalate.sh — escalate() outputs failures to stdout; a silent failure is
# indistinguishable from a quiet-because-nothing-happened pass.
#
#   ./test-skew-escalate.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# escalate() in skew.sh was swallowing both streams of the mail.sh call
# (>/dev/null 2>&1), so a failing send produced no output in skew.log and the
# divergence went unreported for an entire day while the timer ran every hour.
# This suite proves the fix: a failing send, and a missing mail.sh path, both
# produce output on stdout and return non-zero.
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
# covers: spira/skew.sh UC-operator-channel-25
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

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

# A skew.sh runner with an explicit minimal environment. Each call uses its own
# SPIRA_RUN directory so the dedupe stamp never suppresses a second escalation
# in the same test run. HOME is required by conf.sh for the SPIRA_DB default.
#
# run_skew <env-var=val>...        — check --escalate (escalation mode)
# run_skew_ro <env-var=val>...     — check alone (read-only mode; must not call SPIRA_NOTIFY)
# run_skew_shared <run_dir> <env-var=val>... — check --escalate reusing <run_dir> (dedupe test)
run_skew() {
    local run_dir
    run_dir="$(mktemp -d "$TMP/run-XXXXX")"
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$run_dir" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_RELEASES="$RELEASES" \
        "${@}" \
        bash "$HERE/skew.sh" check --escalate 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

run_skew_ro() {
    local run_dir
    run_dir="$(mktemp -d "$TMP/run-XXXXX")"
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$run_dir" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_RELEASES="$RELEASES" \
        "${@}" \
        bash "$HERE/skew.sh" check 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

run_skew_shared() {
    local run_dir="$1"; shift
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$run_dir" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_RELEASES="$RELEASES" \
        "${@}" \
        bash "$HERE/skew.sh" check --escalate 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# ===========================================================================
echo
echo "positive control — successful notify appears on stdout:"
# ===========================================================================
GOOD_HOME="$TMP/good-home"
GOOD_BODY="$TMP/good-body"
mkdir -p "$GOOD_HOME"
cat > "$GOOD_HOME/mail.sh" <<EOFM
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
echo "mail sent"
cat > "$GOOD_BODY"
EOFM
chmod +x "$GOOD_HOME/mail.sh"

good_out="$(run_skew SPIRA_HOME="$GOOD_HOME")"; good_rc=$?
# NOT-LATEST was found; exit 1 is correct.
is  "positive control exits 1 (divergence found)" "1" "$good_rc"
# The escalation confirmation must appear on stdout so skew.log has it.
want "positive control: escalation noted on stdout" "escalated" "$good_out"
# Body must be non-empty — an empty body is indistinguishable from a correct one
# by exit code alone (law-absence-needs-a-positive-control).
[ -s "$GOOD_BODY" ] && ok "body is non-empty" \
    || bad "body is non-empty" "mail.sh received an empty body"
want "body contains ## Question" "## Question" "$(cat "$GOOD_BODY")"
want "body contains ## Default"  "## Default"  "$(cat "$GOOD_BODY")"

# ===========================================================================
echo
echo "no escalation path — warning appears on stdout, not silently dropped:"
# ===========================================================================
NO_HOME="$TMP/no-home"
mkdir -p "$NO_HOME"
# no mail.sh in NO_HOME

no_path_out="$(run_skew SPIRA_HOME="$NO_HOME")"; no_path_rc=$?
is  "no-path exits 1 (divergence found)" "1" "$no_path_rc"
want "no-path warning appears on stdout" "mail.sh not found" "$no_path_out"

# ===========================================================================
echo
echo "failing notify — failure message appears on stdout, returns non-zero:"
# ===========================================================================
BAD_HOME="$TMP/bad-home"
mkdir -p "$BAD_HOME"
cat > "$BAD_HOME/mail.sh" <<'EOF'
#!/usr/bin/env bash
echo "bad-notify: simulated failure from test fixture" >&2
exit 1
EOF
chmod +x "$BAD_HOME/mail.sh"

fail_out="$(run_skew SPIRA_HOME="$BAD_HOME")"; fail_rc=$?
is  "failing notify exits 1" "1" "$fail_rc"
want "failing notify message on stdout"  "escalation failed" "$fail_out"
want "failing notify rc included"        "rc=1"              "$fail_out"
want "failing notify output included"    "simulated failure" "$fail_out"

# ===========================================================================
echo
echo "read-only mode — check without --escalate must not call mail.sh:"
# ===========================================================================
SENTINEL_DIR="$(mktemp -d "$TMP/sentinel-XXXXX")"
SENTINEL_HOME="$TMP/sentinel-home"
mkdir -p "$SENTINEL_HOME"
cat > "$SENTINEL_HOME/mail.sh" <<EOF
#!/usr/bin/env bash
touch "$SENTINEL_DIR/fired"
exit 0
EOF
chmod +x "$SENTINEL_HOME/mail.sh"

ro_out="$(run_skew_ro SPIRA_HOME="$SENTINEL_HOME")"; ro_rc=$?
is  "read-only exits 1 (divergence found)" "1" "$ro_rc"
[ ! -f "$SENTINEL_DIR/fired" ] && ok "read-only: mail.sh not called" \
    || bad "read-only: mail.sh not called" "sentinel file was created"
want   "read-only: NOT-LATEST finding still printed" "NOT-LATEST" "$ro_out"
nowant "read-only: no 'escalated' line"              "escalated"  "$ro_out"
nowant "read-only: no 'mail.sh not found' line"      "mail.sh not found" "$ro_out"

# ===========================================================================
echo
echo "condition-keyed dedupe — same condition, same run dir, not re-escalated:"
# ===========================================================================
DEDUPE_HOME="$TMP/dedupe-home"
DEDUPE_COUNT="$TMP/dedupe-count"
printf '0' > "$DEDUPE_COUNT"
mkdir -p "$DEDUPE_HOME"
cat > "$DEDUPE_HOME/mail.sh" <<EOFN
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
count=\$(cat "$DEDUPE_COUNT" 2>/dev/null || echo 0)
printf '%d' \$((count+1)) > "$DEDUPE_COUNT"
echo "mail sent"
cat >/dev/null
EOFN
chmod +x "$DEDUPE_HOME/mail.sh"

DEDUPE_RUN="$(mktemp -d "$TMP/dedup-run-XXXXX")"

first_out="$(run_skew_shared "$DEDUPE_RUN" SPIRA_HOME="$DEDUPE_HOME")"; first_rc=$?
is  "dedupe first call exits 1 (divergence)" "1" "$first_rc"
want "dedupe first call: escalated" "escalated" "$first_out"
is  "dedupe first call: notify fired once" "1" "$(cat "$DEDUPE_COUNT")"

second_out="$(run_skew_shared "$DEDUPE_RUN" SPIRA_HOME="$DEDUPE_HOME")"; second_rc=$?
is  "dedupe second call exits 1 (divergence still present)" "1" "$second_rc"
nowant "dedupe second call: no re-escalation" "escalated" "$second_out"
is  "dedupe second call: notify still fired exactly once total" "1" "$(cat "$DEDUPE_COUNT")"

# ===========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
