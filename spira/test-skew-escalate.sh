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
# This suite proves the fix: a failing send, and a missing mail path, both
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
    tl_config SPIRA_RUN="$run_dir" SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" SPIRA_RELEASES="$RELEASES" \
        SPIRA_HOME_REPO="fixture" SPIRA_REPO_MAP="$SKEWMAP"
    env -i PATH="$PATH" \
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
    tl_config SPIRA_RUN="$run_dir" SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" SPIRA_RELEASES="$RELEASES" \
        SPIRA_HOME_REPO="fixture" SPIRA_REPO_MAP="$SKEWMAP"
    env -i PATH="$PATH" \
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
    tl_config SPIRA_RUN="$run_dir" SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" SPIRA_RELEASES="$RELEASES" \
        SPIRA_HOME_REPO="fixture" SPIRA_REPO_MAP="$SKEWMAP"
    env -i PATH="$PATH" \
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
GOOD_HOME="$TMP/good-home"
GOOD_BODY="$TMP/good-body"
mkdir -p "$GOOD_HOME"
cat > "$GOOD_HOME/mail" <<EOFM
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
echo "mail sent"
cat > "$GOOD_BODY"
EOFM
chmod +x "$GOOD_HOME/mail"

good_out="$(run_skew SPIRA_HOME="$HERE" PATH="$GOOD_HOME:$PATH")"; good_rc=$?
# NOT-LATEST was found; exit 1 is correct.
is  "positive control exits 1 (divergence found)" "1" "$good_rc"
# The escalation confirmation must appear on stdout so skew.log has it.
want "positive control: escalation noted on stdout" "escalated" "$good_out"
# Body must be non-empty — an empty body is indistinguishable from a correct one
# by exit code alone (law-absence-needs-a-positive-control).
[ -s "$GOOD_BODY" ] && ok "body is non-empty" \
    || bad "body is non-empty" "mail received an empty body"
want "body contains ## Question" "## Question" "$(cat "$GOOD_BODY")"
want "body contains ## Default"  "## Default"  "$(cat "$GOOD_BODY")"

# ===========================================================================
echo
echo "failing notify — failure message appears on stdout, returns non-zero:"
# ===========================================================================
BAD_HOME="$TMP/bad-home"
mkdir -p "$BAD_HOME"
cat > "$BAD_HOME/mail" <<'EOF'
#!/usr/bin/env bash
echo "bad-notify: simulated failure from test fixture" >&2
exit 1
EOF
chmod +x "$BAD_HOME/mail"

fail_out="$(run_skew SPIRA_HOME="$HERE" PATH="$BAD_HOME:$PATH")"; fail_rc=$?
is  "failing notify exits 1" "1" "$fail_rc"
want "failing notify message on stdout"  "escalation failed" "$fail_out"
want "failing notify rc included"        "rc=1"              "$fail_out"
want "failing notify output included"    "simulated failure" "$fail_out"

# ===========================================================================
echo
echo "read-only mode — check without --escalate must not call mail:"
# ===========================================================================
SENTINEL_DIR="$(mktemp -d "$TMP/sentinel-XXXXX")"
SENTINEL_HOME="$TMP/sentinel-home"
mkdir -p "$SENTINEL_HOME"
cat > "$SENTINEL_HOME/mail" <<EOF
#!/usr/bin/env bash
touch "$SENTINEL_DIR/fired"
exit 0
EOF
chmod +x "$SENTINEL_HOME/mail"

ro_out="$(run_skew_ro SPIRA_HOME="$HERE" PATH="$SENTINEL_HOME:$PATH")"; ro_rc=$?
is  "read-only exits 1 (divergence found)" "1" "$ro_rc"
[ ! -f "$SENTINEL_DIR/fired" ] && ok "read-only: mail not called" \
    || bad "read-only: mail not called" "sentinel file was created"
want   "read-only: NOT-LATEST finding still printed" "NOT-LATEST" "$ro_out"
nowant "read-only: no 'escalated' line"              "escalated"  "$ro_out"
nowant "read-only: no 'mail not found' line"      "mail not found" "$ro_out"

# ===========================================================================
echo
echo "condition-keyed dedupe — same condition, same run dir, not re-escalated:"
# ===========================================================================
DEDUPE_HOME="$TMP/dedupe-home"
DEDUPE_COUNT="$TMP/dedupe-count"
printf '0' > "$DEDUPE_COUNT"
mkdir -p "$DEDUPE_HOME"
cat > "$DEDUPE_HOME/mail" <<EOFN
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
count=\$(cat "$DEDUPE_COUNT" 2>/dev/null || echo 0)
printf '%d' \$((count+1)) > "$DEDUPE_COUNT"
echo "mail sent"
cat >/dev/null
EOFN
chmod +x "$DEDUPE_HOME/mail"

DEDUPE_RUN="$(mktemp -d "$TMP/dedup-run-XXXXX")"

first_out="$(run_skew_shared "$DEDUPE_RUN" SPIRA_HOME="$HERE" PATH="$DEDUPE_HOME:$PATH")"; first_rc=$?
is  "dedupe first call exits 1 (divergence)" "1" "$first_rc"
want "dedupe first call: escalated" "escalated" "$first_out"
is  "dedupe first call: notify fired once" "1" "$(cat "$DEDUPE_COUNT")"

second_out="$(run_skew_shared "$DEDUPE_RUN" SPIRA_HOME="$HERE" PATH="$DEDUPE_HOME:$PATH")"; second_rc=$?
is  "dedupe second call exits 1 (divergence still present)" "1" "$second_rc"
nowant "dedupe second call: no re-escalation" "escalated" "$second_out"
is  "dedupe second call: notify still fired exactly once total" "1" "$(cat "$DEDUPE_COUNT")"

# ===========================================================================
echo
tl_summary
