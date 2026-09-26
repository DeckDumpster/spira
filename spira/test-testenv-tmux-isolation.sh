#!/usr/bin/env bash
# test-testenv-tmux-isolation.sh — suites do not see an inherited $TMUX
#
# TMUX_TMPDIR is meant to give a suite its own isolated tmux server. It does
# not when $TMUX is already set: tmux prefers the inherited socket and the
# suite drives the operator's live session. testenv-batch.sh must strip TMUX
# before handing the environment to a suite it launches.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control). Part A proves the
# container CAN see TMUX when it is explicitly injected — so silence in Part B
# is real isolation, not a probe that never checks. Parts A and B skip when
# podman is unavailable; Part C (source checks) always runs.
#
# host-reason: Parts A and B each start a container.
# covers: spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
BATCH="$HERE/testenv-batch.sh"
TESTENV="$HERE/testenv.sh"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-testenv-tmux-isolation.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ===========================================================================
# PART C: source checks — testenv-batch.sh declares the TMUX stripping.
# These run in every environment, including inside a container.
# ===========================================================================
echo
echo "Part C: source checks (always run)"

# POSITIVE CONTROL: the pre-fix RUNNER_VARS loop body does not mention TMUX —
# the matcher must say so, proving it would detect the absence.
_prefx='for _senv_rv in $RUNNER_VARS; do
    [ -n "${!_senv_rv+x}" ] && _suite_env="$_suite_env -u $_senv_rv"
done'
if printf '%s' "$_prefx" | grep -q -- '-u TMUX'; then
    bad "C-ctrl: pre-fix RUNNER_VARS loop does not strip TMUX" \
        "the matcher found TMUX in the pre-fix block — positive control is broken"
else
    ok "C-ctrl: pre-fix RUNNER_VARS loop does not strip TMUX (positive control)"
fi

# testenv-batch.sh must set TMUX= on all four suite exec calls (serial
# with/without timeout, parallel with/without timeout).
_tmux_clears="$(grep -c '"TMUX="' "$HERE/testenv-batch.sh" 2>/dev/null || echo 0)"
[ "${_tmux_clears:-0}" -ge 4 ] \
    && ok "C1: testenv-batch.sh clears TMUX on suite exec calls ($_tmux_clears occurrences)" \
    || bad "C1: testenv-batch.sh clears TMUX on suite exec calls" \
        "found $_tmux_clears occurrence(s), expected >=4"

# ===========================================================================
# Skip container-dependent parts when podman is not available.
# ===========================================================================
if ! command -v podman >/dev/null 2>&1; then
    printf 'SKIP Parts A+B: podman not available — container tests skipped\n' >&2
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    [ "$fail" -eq 0 ] || exit 1
    exit 0
fi

# ===========================================================================
# PART A: POSITIVE CONTROL — a container CAN receive TMUX when explicitly
# injected via podman exec -e, and clears when TMUX= is added after.
# ===========================================================================
echo
echo "Part A: positive control — the container sees TMUX when explicitly passed"

CTRL_CNAME="spira-tmux-ctrl-$$"
_ctrl_cleanup() {
    podman stop "$CTRL_CNAME" >/dev/null 2>&1 || true
    podman rm   "$CTRL_CNAME" >/dev/null 2>&1 || true
}
trap '_ctrl_cleanup; rm -rf "$TMP"' EXIT INT TERM

bash "$TESTENV" up --name "$CTRL_CNAME" >&2 || {
    printf 'SKIP Parts A+B: control container did not start\n' >&2
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    [ "$fail" -eq 0 ] || exit 1
    exit 0
}

_a1_out="$(podman exec --user spirauser \
    -e "XDG_RUNTIME_DIR=/run/user/1001" \
    -e "TMUX=fake-tmux-ctrl-$$" \
    "$CTRL_CNAME" bash -c 'printf "%s" "${TMUX:-}"' 2>/dev/null || true)"
case "$_a1_out" in
    *fake-tmux-ctrl-*) ok "A1: container sees TMUX when -e TMUX=... is passed (positive control)" ;;
    *) bad "A1: container sees TMUX when -e TMUX=... is passed" "got [$_a1_out]" ;;
esac

# -e "TMUX=" added after -e "TMUX=value" clears it — the mechanism the fix uses.
_a2_out="$(podman exec --user spirauser \
    -e "XDG_RUNTIME_DIR=/run/user/1001" \
    -e "TMUX=fake-tmux-ctrl-$$" \
    -e "TMUX=" \
    "$CTRL_CNAME" bash -c 'printf "%s" "${TMUX:-empty}"' 2>/dev/null || true)"
[ "$_a2_out" = "empty" ] \
    && ok "A2: -e TMUX= (later) clears an earlier TMUX injection" \
    || bad "A2: -e TMUX= (later) clears an earlier TMUX injection" "got [$_a2_out]"

_ctrl_cleanup
trap 'rm -rf "$TMP"' EXIT INT TERM

# ===========================================================================
# PART B: testenv-batch.sh — TMUX set on the host does not reach the suite
# ===========================================================================
echo
echo "Part B: testenv-batch.sh strips TMUX from the suite environment"

REMOTE="$TMP/remote"; FIXTURE="$TMP/fixture"
git init -q --initial-branch=master "$REMOTE"
git -C "$REMOTE" config user.email "test@spira.local"
git -C "$REMOTE" config user.name "Spira Test"
touch "$REMOTE/placeholder"
git -C "$REMOTE" add placeholder
git -C "$REMOTE" commit -q -m "initial"
git clone -q --local "$REMOTE" "$FIXTURE"
git -C "$FIXTURE" config user.email "test@spira.local"
git -C "$FIXTURE" config user.name "Spira Test"
git -C "$FIXTURE" checkout -q -b topic
printf 'changed\n' > "$FIXTURE/changed.txt"
git -C "$FIXTURE" add changed.txt
git -C "$FIXTURE" commit -q -m "add changed.txt"
mkdir -p "$FIXTURE/spira"

cat > "$FIXTURE/spira/test-tmux-probe.sh" <<'PROBE'
#!/usr/bin/env bash
# covers: changed.txt
if [ -n "${TMUX:-}" ]; then
    printf "FAIL  TMUX is set in the suite environment: [%s]\n" "$TMUX"
    exit 1
fi
printf "  ok    TMUX is absent from the suite environment\n"
exit 0
PROBE
chmod +x "$FIXTURE/spira/test-tmux-probe.sh"

SUITE_B="$TMP/suites-B"; mkdir -p "$SUITE_B"
cp "$FIXTURE/spira/test-tmux-probe.sh" "$SUITE_B/"

RESULTS_B="$TMP/results-B"
_b_rc=0
TMUX="fake-tmux-from-host-$$" \
SPIRA_BATCH_RESULTS="$RESULTS_B" \
SPIRA_BATCH_SUITE_DIR="$SUITE_B" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="tmux-b-$$" \
    bash "$BATCH" --mode parallel topic "$FIXTURE" 2>/dev/null || _b_rc=$?

_b_dir="$(find "$RESULTS_B" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | head -1)"
if [ -z "$_b_dir" ]; then
    bad "B1: batch produced a results directory" "none (rc=$_b_rc)"
else
    _b_status="$(awk '{print $1}' "$_b_dir/test-tmux-probe.sh.result" 2>/dev/null)"
    [ "$_b_status" = ok ] \
        && ok "B1: probe passed — TMUX was absent in the container" \
        || bad "B1: probe passed" "status=[$_b_status]; $(head -3 "$_b_dir/test-tmux-probe.sh.out" 2>/dev/null | tr '\n' ' ')"
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
