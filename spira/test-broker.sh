#!/usr/bin/env bash
#
# test-broker.sh — broker: one end-to-end smoke of the read dispatch, binary through gh.
#
# The read verbs' gh argv are cargo #[cfg(test)] unit tests in broker/src/read.rs; this proves
# what no in-process test can: that main.rs's dispatch, env propagation and the repo-map
# lookup reach a real gh stub. The retired intent verbs must stay gone from the binary.
#
# tier: T1
# covers: broker/* spira/broker.sh UC-landing-merge-queue-55
# hermetic-ok: no database required; gh is a stub; czar-fence.sh runs against a fixture env
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1: did not want [$2] in [$3]"; }

echo "test-broker.sh"



# Scratch space: also used if we need to build broker from a writable copy.
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# (this crate's own #[test]s run once per round as the workspace unit-test step, not here.)

# =========================================================================
echo
echo "POSITIVE CONTROL — broker binary exists and is executable"
# =========================================================================
# broker is the tree's own build, invoked by name on the suite's PATH (sp-gypjk).
BROKER_DIR="$(command -v broker)" && BROKER_DIR="$(dirname "$BROKER_DIR")"
if [ -n "$BROKER_DIR" ]; then
    ok "broker binary is present and executable"
else
    bad "broker is not on PATH (positive control: fails before the fix)"
    printf '\n0 passed, 1 failed\n'
    exit 1
fi

# =========================================================================
# SHARED FIXTURE
# =========================================================================

# Minimal git repo for the broker to cd into (repo-map needs a real directory).
REPO_DIR="$T/fake-repo"
git init -q "$REPO_DIR"

# Repo-map: maps label "test-repo" to the fake repo directory.
REPO_MAP="$T/repo-map"
printf 'test-repo | %s | push | origin/main | | \n' "$REPO_DIR" > "$REPO_MAP"

# Stub gh: records what it was called with and exits 0.
GH_STUB="$T/gh"
cat > "$GH_STUB" << 'ENDGH'
#!/usr/bin/env bash
printf 'stub-gh: %s\n' "$*" >> "$SPIRA_BROKER_GH_LOG"
exit 0
ENDGH
chmod +x "$GH_STUB"
GH_LOG="$T/gh.log"

SPIRA_RUN="$T/run"
mkdir -p "$SPIRA_RUN"
tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_REPO_MAP="$REPO_MAP" SPIRA_BD="true"

base_env() {
    env -i \
        PATH="$BROKER_DIR:$HERE:/usr/local/bin:/usr/bin:/bin" \
        HOME="$T/home" \
        SPIRA_HOME="$HERE" \
        SPIRA_BROKER_GH="$GH_STUB" \
        SPIRA_BROKER_GH_LOG="$GH_LOG" \
        SPIRA_TOML="$SPIRA_TOML" \
        "$@"
}

# =========================================================================
echo
echo "RETIRED — submit and execute are no longer verbs of the binary."
# =========================================================================
for retired in submit execute; do
    retired_out="$(base_env broker "$retired" 2>&1)"; retired_rc=$?
    is "broker $retired is refused as usage" "2" "$retired_rc"
    want "broker $retired usage names read and token only" "broker read" "$retired_out"
    lack "broker $retired usage does not offer $retired" "broker $retired" "$retired_out"
done

# =========================================================================
echo
echo "SMOKE — broker read run-view: main.rs's read dispatch reaches gh and returns."
# =========================================================================
> "$GH_LOG"
cat > "$GH_STUB" << 'ENDGH2'
#!/usr/bin/env bash
printf 'stub-gh: %s\n' "$*" >> "$SPIRA_BROKER_GH_LOG"
case "$1 $2" in
    "run view") printf '{"status":"completed","conclusion":"success","databaseId":12345}\n' ;;
esac
exit 0
ENDGH2
chmod +x "$GH_STUB"
read_out="$(base_env broker read run-view test-repo/12345 2>&1)"
is "broker read run-view exits 0" "0" "$?"
want "broker read run-view output contains status" '"status"' "$read_out"
want "gh called with run view 12345" "run view 12345" "$(cat "$GH_LOG" 2>/dev/null || echo "")"

# =========================================================================
echo
tl_summary
