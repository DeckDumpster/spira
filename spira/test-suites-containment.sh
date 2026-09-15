#!/usr/bin/env bash
#
# test-suites-containment.sh — a timed pass cannot reach the host it was launched from
#
#   ./test-suites-containment.sh
#
# THE DEFECT THIS SUITE GUARDS AGAINST. law-tests-run-only-through-testenv-batch has been in
# force while nothing obeyed it: suites.sh ran every suite as `bash "$HERE/$s"` on the host,
# and so did the gate. testenv-batch.sh existed, was 40KB, was fully tested, and had no
# production caller at all — every caller was a suite testing testenv-batch itself.
#
# What that cost: a timed pass launched from inside a tmux pane. Suites isolate their tmux
# server with TMUX_TMPDIR, but tmux resolves $TMUX ahead of it, so test-cockpit-rebuild.sh
# drove rebuild.sh against the operator's live server and destroyed their cockpit mid-session;
# test-uninstall.sh was still queued behind it. In a container none of that is reachable
# whatever $TMUX says, which is the entire point of the statute.
#
# WHY A SENTINEL FILE AND NOT AN ASSERTION ABOUT THE RUNNER. "suites.sh calls testenv-batch.sh"
# is a claim about the source; it stays true while a later edit adds a fallback that quietly
# runs on the host when the container will not start. The property worth holding is not which
# program is invoked, it is that a suite in the pass CANNOT SEE THE HOST. So this plants a
# file on the host, plants a suite that looks for it, and requires the pass to report that
# suite green — green meaning the suite could not find it.
#
# POSITIVE CONTROL IS FIRST AND IS NOT OPTIONAL (law-absence-needs-a-positive-control). A
# suite that cannot see a host file and a suite that never ran produce the same verdict from
# outside, and the wrong one reads as containment. So the same planted suite is first run on
# the host, where it MUST find the sentinel and fail. Only once the probe has been seen to
# fail does its passing inside the pass mean anything.
#
# covers: spira/suites.sh spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-suites-containment.sh"

# A container is the whole subject of this suite. Without podman there is nothing to assert,
# and asserting nothing must not read as a pass.
command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-suites-containment.sh: podman not available — containment cannot be tested\n' >&2
    exit 77
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# The sentinel lives somewhere a container does not mount. /tmp is bind-mounted in some
# testenv layouts, so the operator's run directory is the honest choice: if a suite can read
# this path, it is reading the real box.
SENTINEL="$TMP/host-sentinel-$$"
printf 'if a suite can read this, it is on the host\n' > "$SENTINEL"
[ -r "$SENTINEL" ] || { echo "test-suites-containment: could not plant the sentinel"; exit 1; }

# The probe suite: exits 1 when it can see the host sentinel, 0 when it cannot.
PROBE="$TMP/test-containment-probe.sh"
cat > "$PROBE" <<EOF
#!/usr/bin/env bash
# covers: spira/suites.sh
set -uo pipefail
if [ -r "$SENTINEL" ]; then
    echo "FAIL  the host sentinel is readable — this suite is running on the host"
    echo "ASSERTIONS 1"
    exit 1
fi
echo "ok    the host sentinel is not readable"
echo "ASSERTIONS 1"
exit 0
EOF
chmod +x "$PROBE"

# ======================================================================================
echo
echo "POSITIVE CONTROL — the probe must FAIL when it really is on the host:"
# ======================================================================================
# If this passes, the probe cannot tell host from container and every assertion below is
# meaningless — it would report containment against a check that never looks at anything.
bash "$PROBE" >/dev/null 2>&1
_host_rc=$?
if [ "$_host_rc" -ne 0 ]; then
    ok "the probe sees the sentinel on the host (rc=$_host_rc)"
else
    bad "the probe sees the sentinel on the host" \
        "it exited 0 on the host — the probe cannot distinguish host from container"
fi

# ======================================================================================
echo
echo "the same probe, run through testenv-batch.sh, cannot see it:"
# ======================================================================================
# The probe has to live in the suite directory for the runner to find it by name, so it is
# copied in and removed again. A stray test-*.sh left in spira/ would join every later pass
# by existing, which is how suites are discovered here.
_installed="$HERE/test-containment-probe.sh"
cp "$PROBE" "$_installed"
trap 'rm -f "$_installed"; rm -rf "$TMP"' EXIT INT TERM

_batch_root="$TMP/batch-results"
printf 'test-containment-probe.sh\n' \
    | SPIRA_BATCH_RESULTS="$_batch_root" \
      bash "$HERE/testenv-batch.sh" --suites - HEAD > "$TMP/batch.log" 2>&1
_batch_rc=$?

_res_dir="$(find "$_batch_root" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | head -1)"
if [ -z "$_res_dir" ]; then
    bad "the batch produced a results directory" \
        "none under $_batch_root (rc=$_batch_rc) — see $TMP/batch.log; containment is UNTESTED, not proven"
else
    _res="$_res_dir/test-containment-probe.sh.result"
    if [ ! -r "$_res" ]; then
        # A selected suite with no result was not reached. That is not a pass.
        bad "the probe produced a result" "no result file at $_res — the suite never ran"
    else
        _status="$(awk '{print $1}' "$_res" 2>/dev/null)"
        if [ "$_status" = ok ]; then
            ok "the probe cannot read the host sentinel from inside the container"
        else
            bad "the probe cannot read the host sentinel from inside the container" \
                "status=[$_status]; $(head -3 "$_res_dir/test-containment-probe.sh.out" 2>/dev/null | tr '\n' ' ')"
        fi
    fi
fi

# ======================================================================================
echo
echo "suites.sh delegates rather than running on the host:"
# ======================================================================================
# Weaker than the sentinel assertions above and deliberately kept alongside them: this one
# names the mechanism, so a change that removes delegation is reported as itself rather than
# only as a containment failure somewhere downstream.
if grep -q 'testenv-batch.sh" --suites -' "$HERE/suites.sh" 2>/dev/null; then
    ok "suites.sh hands its selection to testenv-batch.sh"
else
    bad "suites.sh hands its selection to testenv-batch.sh" "no --suites - invocation in suites.sh"
fi

# The inline escape must remain exactly that — an escape for passes that are already
# contained, never a fallback taken when the container fails to start.
if grep -q 'no results directory' "$HERE/suites.sh" 2>/dev/null; then
    ok "a missing results directory fails the pass rather than reading as green"
else
    bad "a missing results directory fails the pass rather than reading as green" \
        "suites.sh does not refuse a batch that produced no results"
fi

# ======================================================================================
echo
echo "the batch verdict words map onto the right exit codes:"
# ======================================================================================
# testenv-batch.sh writes two different skip words and they mean different things: `skip` is
# the suite exiting 77 for itself, `skip-req` is the runner refusing to start it because a
# `# requires:` token is absent from the image. Both are a suite that correctly declined to
# run. The first batch-mode pass mapped only `skip` and let `skip-req` fall through to the
# default arm, which is rc=1 — so every requires-gated suite was filed as a red bead
# (law-alerts-must-be-actionable). Asserted on the source because the mapping only executes
# with a real container behind it, and a source check that cannot fail is worth nothing: the
# control below plants the pre-fix text and requires the matcher to reject it.
_map="$(sed -n '/case "\$_br_status" in/,/esac/p' "$HERE/suites.sh")"
if [ -z "$_map" ]; then
    bad "the batch status mapping is readable" "no case block on _br_status in suites.sh"
else
    if printf '%s' "$_map" | grep -qE '^\s*skip\|skip-req\)'; then
        ok "skip and skip-req both map to 77"
    else
        bad "skip and skip-req both map to 77" \
            "skip-req is not on the skip arm — it falls through to the default and reads as a failure"
    fi
    # POSITIVE CONTROL: the same matcher against the text as it stood before the fix must
    # say no. Without this, a matcher broken into always-true would report the mapping fixed.
    if printf '%s' '            skip)     rc=77 ;;' | grep -qE '^\s*skip\|skip-req\)'; then
        bad "control — the pre-fix mapping is rejected" \
            "the matcher accepted the old skip-only arm; it cannot tell the two apart"
    else
        ok "control — the pre-fix skip-only arm is rejected by the same matcher"
    fi
fi

# ======================================================================================
echo
echo "every suite that EXECUTES suites.sh declares the inline escape:"
# ======================================================================================
# suites.sh delegates its pass to a container unless SPIRA_SUITES_INLINE=1 is set. A suite
# that drives suites.sh against fake suites in a scratch tree must set it, or the run it is
# asserting about never happens: the containerized pass runs a different tree, files nothing
# into the fixture database, and every "a bead was filed" assertion reads 0 — which is how
# test-output-visibility.sh and test-watchtower.sh went red when the delegation landed. The
# escape was added to the nine suites matching test-suites-*.sh and missed everything else,
# because the set was chosen by NAME rather than by what the file actually calls.
#
# So the set is derived here from the call, not from a list (law-a-runner-takes-a-list: the
# list is what goes stale). A suite qualifies when a NON-COMMENT line EXECUTES suites.sh and
# the subcommand is either run/once or a variable — the wrappers in these suites are
# `sut() { local cmd="$1"; ... bash "$SH/suites.sh" "$cmd"; }`, so matching a literal "run"
# next to the path finds none of them.
#
# Three things are deliberately NOT matched, each of which a looser pattern caught wrongly
# on the way to this one: `suites.sh status` (read-only, starts nothing), the string
# "suites.sh run" inside an assertion about a systemd unit's ExecStart, and a bare mention
# in a for-loop list of filenames. All three would be reported as suites needing an escape
# they have no use for.
_esc_missing=""
_esc_checked=0
for _f in "$HERE"/test-*.sh; do
    grep -vE '^[[:space:]]*#' "$_f" 2>/dev/null \
        | grep -qE '(bash|exec)[[:space:]]+"[^"]*suites\.sh"[[:space:]]+("\$|\$\{?[A-Za-z_]|run\b|once\b)' \
        || continue
    _esc_checked=$((_esc_checked + 1))
    grep -q 'SPIRA_SUITES_INLINE' "$_f" || _esc_missing="$_esc_missing $(basename "$_f")"
done

# POSITIVE CONTROL: the detector must have found the suites that do this. Zero would mean
# the matcher, not the tree, is clean — and would report every suite as compliant.
if [ "$_esc_checked" -ge 9 ]; then
    ok "the detector found $_esc_checked suites that execute suites.sh"
else
    bad "the detector found suites that execute suites.sh" \
        "only $_esc_checked matched — the compliance check below proves nothing"
fi
if [ -z "$_esc_missing" ]; then
    ok "all of them set SPIRA_SUITES_INLINE"
else
    bad "all of them set SPIRA_SUITES_INLINE" \
        "missing the escape, so their pass runs in a container against a different tree:$_esc_missing"
fi

echo
echo "test-suites-containment.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
