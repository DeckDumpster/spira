#!/usr/bin/env bash
# covers: spira/suites.sh spira/suite-state.sh spira/suite-state
# A red suite is reported and attributed, never silently quarantined (sp-n2ax8): observe-flake
# crossing threshold files a bead through incident.sh and touches nothing else — no suite-state
# write, no branch, no queue submission.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"
. "$HERE/suite-state.sh"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# POSITIVE CONTROL — prove the test detects a direct write to the checkout.
# A write that bypasses the reporting path IS caught by git status --porcelain.
# Without this control, "checkout is clean" could be true because nothing ran.
# ---------------------------------------------------------------------------
_pdir="$TMP/positive-repo"
mkdir -p "$_pdir/spira"
git init -q "$_pdir"
git -C "$_pdir" config user.email "flake-suite@example.invalid"
git -C "$_pdir" config user.name "Test"
: > "$_pdir/spira/suite-state"
git -C "$_pdir" add spira/suite-state
git -C "$_pdir" commit -q --no-gpg-sign -m "init"
suite_state_write "$_pdir/spira/suite-state" "test-fake.sh" quarantined "" "direct"
_d="$(git -C "$_pdir" status --porcelain -- spira/suite-state 2>/dev/null)"
[ -n "$_d" ] && ok "positive control: direct write dirtied the checkout" \
    || bad "positive control: direct write was not detected by git status"

# ---------------------------------------------------------------------------
# FIXTURE — a scratch git repo acts as the harness checkout.
# suites.sh is run from a scratch spira/ directory inside that repo.
# ---------------------------------------------------------------------------
_repo="$TMP/harness"
_sh="$_repo/spira"
_state="$TMP/state"
_run="$TMP/run"
_home="$TMP/home"
_cap="$TMP/captures"
mkdir -p "$_sh" "$_state" "$_run" "$_home" "$_cap"

git init -q "$_repo"
git -C "$_repo" config user.email "flake-suite@example.invalid"
git -C "$_repo" config user.name "Test"

for _f in suites.sh lib.sh conf.sh suite-state.sh suite-covers.sh; do
    cp "$HERE/$_f" "$_sh/"
done

# Stub incident.sh: record argv/env/stdin (keyed on the ref, so calls never collide),
# answer with a fake id on the last line — the one contract file_flake parses.
cat > "$_sh/incident.sh" <<'STUB'
#!/usr/bin/env bash
_ref="${SPIRA_INCIDENT_REF:-noref}"
_safe="$(printf '%s' "$_ref" | tr -c 'A-Za-z0-9_.-' '_')"
{
    printf 'ARGV: %s\n' "$*"
    env | grep -E '^SPIRA_(INCIDENT|SIN)_' | sort
    printf -- '--- stdin ---\n'
    cat
} > "$SPIRA_TEST_CAP_DIR/$_safe"
printf 'sp-stubfake1\n'
STUB
chmod +x "$_sh/incident.sh"

# Empty gate-suites: all suites are timed (needed by gated_suites()).
: > "$_sh/gate-suites"

# Stub suite: must exist so the existence check in cmd_observe_flake passes.
printf '#!/usr/bin/env bash\n: # stub\n' > "$_sh/test-flaky.sh"
chmod +x "$_sh/test-flaky.sh"

# Initial commit with an empty suite-state so the repo has a clean HEAD.
: > "$_sh/suite-state"
git -C "$_repo" add spira/
git -C "$_repo" commit -q --no-gpg-sign -m "init"

cap_of() { cat "$_cap/$1" 2>/dev/null || true; }

# obs — run observe-flake in an explicit minimal environment.
# SPIRA_SUITES_INLINE=1 prevents the batch delegation path; observe-flake never
# runs suites, so the delegation never fires, but declaring it avoids the
# container probe that would reach the host Docker daemon.
obs() {
    env -i \
        PATH="$PATH" \
        HOME="$_home" \
        SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$_sh" \
        SPIRA_REPO="$_repo" \
        SPIRA_RUN="$_run" \
        SPIRA_SUITES_STATE="$_state" \
        SPIRA_FLAKE_QUARANTINE_AT=2 \
        SPIRA_FLAKE_WINDOW=604800 \
        SPIRA_SUITES_INLINE=1 \
        SPIRA_TEST_CAP_DIR="$_cap" \
        bash "$_sh/suites.sh" observe-flake test-flaky.sh "$1" 2>&1
}

# First observation: below threshold.  Production checkout must stay clean, no report filed.
obs run-1 >/dev/null
_d="$(git -C "$_repo" status --porcelain -- spira/suite-state 2>/dev/null)"
[ -z "$_d" ] && ok "observe(1): production checkout clean below threshold" \
    || bad "observe(1): production checkout dirty after one observation: '$_d'"
[ -z "$(cap_of flake_test-flaky.sh)" ] && ok "observe(1): below threshold, no report filed" \
    || bad "observe(1): a report was filed below threshold"

# Second observation: crosses threshold.  Reported through incident.sh; nothing else changes.
out2="$(obs run-2)"
_d="$(git -C "$_repo" status --porcelain -- spira/suite-state 2>/dev/null)"
[ -z "$_d" ] && ok "observe(2): production checkout stays clean at threshold" \
    || bad "observe(2): production checkout dirty after threshold: '$_d'"

# No branch of any kind is created — the whole mechanism is gone, not just its target.
_branch="$(git -C "$_repo" branch --list 'spira-suite-state/*' | head -1 | tr -d ' *')"
[ -z "$_branch" ] && ok "observe(2): no branch created" \
    || bad "observe(2): a branch was created: $_branch"

# The suite is never written to suite-state — it is not quarantined by this path.
_st="$(suite_state_of "$_sh/suite-state" "test-flaky.sh")"
[ "$_st" = "active" ] && ok "observe(2): suite-state never written (stays active)" \
    || bad "observe(2): suite-state shows '$_st', expected active"

_c="$(cap_of flake_test-flaky.sh)"
[ -n "$_c" ] && ok "observe(2): report filed through incident.sh" \
    || bad "observe(2): no report was filed at threshold"
want "observe(2): ref is flake:<suite>" "SPIRA_INCIDENT_REF=flake:test-flaky.sh" "$_c"
want "observe(2): cause is suite-flaky" "SPIRA_INCIDENT_CAUSE=suite-flaky" "$_c"
want "observe(2): sin-exempt (recurring flakes dedupe, never escalate alone)" "SPIRA_SIN_EXEMPT=1" "$_c"
want "observe(2): filing named the question, not a quarantine" "why does test-flaky.sh fail intermittently" "$_c"
want "observe(2): output reports the filed bead id" "reported (bead: sp-stubfake1)" "$out2"

tl_summary
