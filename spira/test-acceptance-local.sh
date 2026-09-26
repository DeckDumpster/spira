#!/usr/bin/env bash
# tier: T2
# covers: spira/acceptance-local.sh UC-instance-lifecycle-46 UC-instance-lifecycle-47
# host-reason: acceptance-local.sh drives its own testenv container (build-tarball.sh
#   and the real thing under test both run outside any nesting); mirrors
#   test-testenv-batch-branch.sh's own declaration for the same reason.
#
# WHAT THIS TESTS. Not the real toolchain build or a real release — those are
# acceptance-local.sh's own dependencies (build-tarball.sh, acceptance-run.sh),
# each covered by their own suite. This suite drives acceptance-local.sh against
# a fake tree whose spira/build-tarball.sh and spira/acceptance-run.sh are stubs,
# so the fast, deterministic thing under test is acceptance-local.sh's OWN
# plumbing: it builds (stub), stands up a REAL container, copies the tarball in,
# wires the scratch repo and the --tarball/--agent arguments, propagates the
# real exit code, and copies forensics out on FAIL only.
#
# SKIP CONDITION: no podman on PATH.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
SCRIPT="$HERE/acceptance-local.sh"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-acceptance-local.sh: podman not found on PATH\n' >&2
    exit 77
}

echo "test-acceptance-local.sh"

echo
echo "1. POSITIVE CONTROL — acceptance-local.sh exists and is executable"

if [ -f "$SCRIPT" ] && [ -x "$SCRIPT" ]; then
    ok "acceptance-local.sh exists and is executable"
else
    bad "acceptance-local.sh exists and is executable" "missing or not executable"
    tl_summary
fi

SCRATCH="$(mktemp -d)"
CNAME="acc-local-test-$$"
trap 'bash "$HERE/testenv.sh" down --name "$CNAME" >/dev/null 2>&1; rm -rf "$SCRATCH"' EXIT INT TERM

# ===========================================================================
echo
echo "2. Usage errors exit 2 before anything real starts"
# ===========================================================================

bash "$SCRIPT" >/dev/null 2>&1
wantrc "no argument -> exit 2" "2" "$?"

bash "$SCRIPT" "$SCRATCH/does-not-exist" >/dev/null 2>&1
wantrc "nonexistent tree -> exit 2" "2" "$?"

NOT_SPIRA="$SCRATCH/not-a-spira-tree"
mkdir -p "$NOT_SPIRA"
bash "$SCRIPT" "$NOT_SPIRA" >/dev/null 2>&1
wantrc "tree with no spira/build-tarball.sh -> exit 2" "2" "$?"

podman container exists "$CNAME" 2>/dev/null \
    && bad "usage errors never start a container" "container $CNAME exists" \
    || ok "usage errors never start a container"

# ===========================================================================
echo
echo "3. Fixture: a fake tree whose build-tarball.sh and acceptance-run.sh are stubs"
# ===========================================================================
# build-tarball.sh and acceptance-run.sh are each covered by their own suite
# (test-tarball-bins.sh / test-workspace-dist.sh, test-acceptance-run.sh and
# test-acceptance-lib.sh's #10). Standing those up for real here would make
# this suite re-verify someone else's mechanism instead of acceptance-local.sh's
# own — the fixture stands in for both, deterministically and fast.

FT="$SCRATCH/fake-tree"
mkdir -p "$FT/spira"

cat > "$FT/Makefile" <<'EOF'
build:
	@true
EOF

cat > "$FT/spira/build-tarball.sh" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail
_out="."
while [ $# -gt 0 ]; do
    if [ "$1" = "--output" ]; then _out="$2"; shift 2; continue; fi
    shift
done
mkdir -p "$_out"
printf 'fake tarball\n' > "$_out/spira-fake.tar.gz"
printf '%s/spira-fake.tar.gz\n' "$_out"
EOF
chmod +x "$FT/spira/build-tarball.sh"

# The stub `acceptance-run.sh` runs INSIDE the container (mounted at /workspace):
# records its argv, mirrors SPIRA_ACCEPTANCE_FORENSICS the way the real script
# does (a marker file, proving the copy-out step moves real data, not an empty
# directory), and exits with whatever /workspace/.stub-rc says (default 0) — the
# host writes that file before each invocation to control PASS vs FAIL.
cat > "$FT/spira/acceptance-run.sh" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail
printf '%s\n' "$*" > /workspace/.stub-args
if [ -n "${SPIRA_ACCEPTANCE_FORENSICS:-}" ]; then
    mkdir -p "$SPIRA_ACCEPTANCE_FORENSICS"
    printf 'stub forensics\n' > "$SPIRA_ACCEPTANCE_FORENSICS/marker.txt"
fi
_rc=0
[ -f /workspace/.stub-rc ] && _rc="$(cat /workspace/.stub-rc)"
exit "$_rc"
EOF
chmod +x "$FT/spira/acceptance-run.sh"

printf '#!/usr/bin/env bash\nexit 0\n' > "$FT/spira/acceptance-agent.sh"
chmod +x "$FT/spira/acceptance-agent.sh"

ok "fixture tree built at $FT"

# ===========================================================================
echo
echo "4. PASS: stub exits 0 -> acceptance-local.sh exits 0, no forensics copied"
# ===========================================================================

rm -f "$FT/.stub-rc" "$FT/.stub-args"
printf '0\n' > "$FT/.stub-rc"
FORENSICS_PASS="$SCRATCH/forensics-pass"
_rc=0
SPIRA_ACCEPTANCE_LOCAL_NAME="$CNAME" \
    SPIRA_ACCEPTANCE_LOCAL_FORENSICS="$FORENSICS_PASS" \
    bash "$SCRIPT" "$FT" >"$SCRATCH/pass.out" 2>&1 || _rc=$?
wantrc "stub PASS -> acceptance-local.sh exits 0" "0" "$_rc"
[ -d "$FORENSICS_PASS" ] \
    && bad "PASS: forensics not copied" "$FORENSICS_PASS exists" \
    || ok "PASS: forensics not copied"

# ===========================================================================
echo
echo "5. FAIL: stub exits 1 -> acceptance-local.sh exits 1, forensics copied out"
# ===========================================================================

rm -f "$FT/.stub-rc" "$FT/.stub-args"
printf '1\n' > "$FT/.stub-rc"
FORENSICS_FAIL="$SCRATCH/forensics-fail"
_rc=0
SPIRA_ACCEPTANCE_LOCAL_NAME="$CNAME" \
    SPIRA_ACCEPTANCE_LOCAL_FORENSICS="$FORENSICS_FAIL" \
    bash "$SCRIPT" "$FT" >"$SCRATCH/fail.out" 2>&1 || _rc=$?
wantrc "stub FAIL -> acceptance-local.sh exits 1" "1" "$_rc"
is "FAIL: forensics copied out with the real marker (not an empty dir)" \
    "stub forensics" "$(cat "$FORENSICS_FAIL/marker.txt" 2>/dev/null || true)"

# ===========================================================================
echo
echo "6. Argument wiring: --tarball, --agent, --scratch-repo present; no --prev-tag"
# ===========================================================================
# Phase A only: a local rehearsal has no predecessor to upgrade from.

_stub_args="$(cat "$FT/.stub-args" 2>/dev/null || true)"
want "stub received --tarball (download skipped)" "--tarball" "$_stub_args"
want "stub received --agent pointing at the mounted tree" \
    "--agent /workspace/spira/acceptance-agent.sh" "$_stub_args"
want "stub received --scratch-repo" "--scratch-repo" "$_stub_args"
nowant "no --prev-tag: only phase A runs locally" "--prev-tag" "$_stub_args"

# ===========================================================================
echo
echo "7. Container is torn down after the run either way"
# ===========================================================================

podman container exists "$CNAME" 2>/dev/null \
    && bad "container torn down after a FAIL run" "container $CNAME still exists" \
    || ok "container torn down after a FAIL run"

tl_summary
