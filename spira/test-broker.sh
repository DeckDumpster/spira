#!/usr/bin/env bash
#
# test-broker.sh — broker: one end-to-end smoke, binary through execute through gh.
#
# WHAT MOVED OUT. The policy table (fayth × verb refusal), intent JSON field shape, and gh
# argv per verb are cargo #[cfg(test)] unit tests in broker/src/{policy,submit,execute,
# read}.rs — see UC-landing-merge-queue-55 — run below via `cargo test -p broker`. The
# end-to-end smoke that follows proves what no in-process unit test can: that main.rs's
# dispatch, real file I/O under SPIRA_RUN, and env var propagation to a real gh stub actually
# wire together. The conf.sh/build.sh/broker.sh declaration greps moved to
# broker-allowlist-lint.sh, which runs without cargo.
#
# THE "FAILS OPEN" CASE IS GONE. It called the gh stub directly, bypassing the broker
# entirely, to "prove what the broker prevents" — but a check that always passes regardless
# of the broker's own correctness proves nothing about this suite's subject, only that gh
# stubs can be invoked. What actually matters (a refused intent never reaches gh) is already
# asserted below, on the real path, with a real refusal.
#
# tier: T1
# covers: broker/* spira/broker.sh
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

audit="$SPIRA_RUN/broker/audit.jsonl"

# =========================================================================
echo
echo "SMOKE — submit through execute, refused: a builder fayth cannot reach gh for a"
echo "czar-only verb. The policy DECISION is Rust-tested; this proves it is actually wired"
echo "into the running binary end to end."
# =========================================================================
intent_id="$(base_env \
    FAYTH_NAME=builder \
    SPIRA_AEON=test-aeon \
    broker submit run-rerun test-repo/99 \
        --reason "test reason" --bead sp-test --class deadlock 2>&1)"
is "submit exits 0" "0" "$?"
[ -f "$SPIRA_RUN/broker/inbox/${intent_id}.json" ] \
    && ok "submit writes the intent to inbox/" \
    || bad "submit writes the intent to inbox/" "no such file"

> "$GH_LOG"
tl_config SPIRA_CZAR_STAGE_DEADLOCK=act
base_env broker execute >/dev/null 2>&1

refused_line="$(grep "$intent_id" "$audit" 2>/dev/null || echo "")"
want "builder+czar-only verb is REFUSED end to end" '"REFUSED"' "$refused_line"
lack "gh is never called for the refused intent"    "99"        "$(cat "$GH_LOG" 2>/dev/null || echo "")"
[ -f "$SPIRA_RUN/broker/refused/${intent_id}.json" ] \
    && ok "the refused intent is moved to refused/" \
    || bad "the refused intent is moved to refused/" "not found"

# =========================================================================
echo
echo "SMOKE — submit through execute, done: czar + act reaches gh with the real argv."
# =========================================================================
> "$GH_LOG"
act_id="$(base_env \
    FAYTH_NAME=czar \
    SPIRA_AEON=czar-1 \
    broker submit run-rerun test-repo/777 \
        --reason "act rerun" --bead sp-act --class deadlock 2>&1)"
tl_config SPIRA_CZAR_STAGE_DEADLOCK=act
base_env broker execute >/dev/null 2>&1

act_line="$(grep "$act_id" "$audit" 2>/dev/null || echo "")"
want "czar+act run-rerun → DONE end to end" '"DONE"' "$act_line"
gh_calls="$(cat "$GH_LOG" 2>/dev/null || echo "")"
want "gh received the run rerun argv" "run rerun 777" "$gh_calls"
[ -f "$SPIRA_RUN/broker/done/${act_id}.json" ] \
    && ok "the done intent is moved to done/" \
    || bad "the done intent is moved to done/" "not found"

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
