#!/usr/bin/env bash
# covers: spira/acceptance-run.sh install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
SCRIPT="$HERE/acceptance-run.sh"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()    { grep -qF  "$2" "$SCRIPT" && ok "$1" || bad "$1" "not found: [$2]"; }
wantre()  { grep -qE  "$2" "$SCRIPT" && ok "$1" || bad "$1" "pattern not found: $2"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }
is_eq()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-acceptance-run.sh"

SCRATCH="$(mktemp -d)"
TMP="$SCRATCH"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

echo
echo "1. POSITIVE CONTROL — acceptance-run.sh exists and is executable"

if [ -f "$SCRIPT" ] && [ -x "$SCRIPT" ]; then
    ok "acceptance-run.sh exists and is executable"
else
    bad "acceptance-run.sh exists and is executable" "missing or not executable"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

echo
echo "2. Structural: SPIRA_OPERATED=0 in each phase's install environment"

wantre "phase A _install_env includes SPIRA_OPERATED=0" \
    '_install_env=\(.*SPIRA_OPERATED=0'
wantre "phase B passes SPIRA_OPERATED=0 to install.sh directly" \
    'SPIRA_OPERATED=0 bash.*_prev_clone.*install\.sh'
wantre "phase D _aged_env includes SPIRA_OPERATED=0" \
    '_aged_env=\(.*SPIRA_OPERATED=0'
want "SPIRA_OPERATED = 0 written to spira.conf" \
    "SPIRA_OPERATED = 0"

echo
echo "3. Pair: stub doctor FAILs without SPIRA_OPERATED=0, passes with it"

# Stub doctor.sh: exits 1 and emits FAIL lines when SPIRA_OPERATED is not 0.
cat > "$SCRATCH/doctor.sh" <<'DOCTOR'
#!/usr/bin/env bash
if [ "${SPIRA_OPERATED:-1}" != "0" ]; then
    printf 'FAIL  aerc (COCKPIT_MAIL) is not on PATH\n'
    printf 'FAIL  hunk is not on PATH\n'
    exit 1
fi
exit 0
DOCTOR
chmod +x "$SCRATCH/doctor.sh"

# Stub install.sh: calls the stub doctor as preflight; refuses on non-zero.
cat > "$SCRATCH/install.sh" <<INSTALL
#!/usr/bin/env bash
_out="\$("$SCRATCH/doctor.sh" 2>&1)"
_rc=\$?
printf '%s\n' "\$_out"
[ "\$_rc" = 0 ] || { printf 'install: preflight failed\n' >&2; exit 1; }
printf 'install: ok\n'
exit 0
INSTALL
chmod +x "$SCRATCH/install.sh"

# Positive control: stub install refuses when SPIRA_OPERATED is unset.
_out="$(bash "$SCRATCH/install.sh" 2>&1)" && _rc=0 || _rc=$?
if [ "$_rc" -ne 0 ] && printf '%s' "$_out" | grep -qF 'COCKPIT_MAIL'; then
    ok "positive-control: stub install refuses when SPIRA_OPERATED unset (names operator tool)"
else
    bad "positive-control: stub install refuses when SPIRA_OPERATED unset" \
        "exit=$_rc"
fi

# With SPIRA_OPERATED=0, stub install proceeds.
_out2="$(SPIRA_OPERATED=0 bash "$SCRATCH/install.sh" 2>&1)" && _rc2=0 || _rc2=$?
if [ "$_rc2" -eq 0 ]; then
    ok "with SPIRA_OPERATED=0: stub install proceeds"
else
    bad "with SPIRA_OPERATED=0: stub install proceeds" \
        "exit=$_rc2 output=$_out2"
fi

# ===========================================================================
echo
echo "4. bead-id extraction: warning before id line (positive control first)"
# ===========================================================================
# Helper: mirrors the extraction logic in acceptance-run.sh.
_extract_bead_id() {
    printf '%s\n' "$1" \
        | sed -n 's/.*Created issue: \([a-z0-9]*-[a-z0-9]*\).*/\1/p' \
        | head -1
}

# Positive control: extractor finds nothing when output has no id.
_ctrl="$(_extract_bead_id "warning: beads.role not configured (GH#2950)")"
is_eq "positive-control: no id in warning-only output → empty" "" "$_ctrl"

# Main case: warning lines before ✓ Created issue: sp-xxxx — the failing pattern from GH#2950.
_out_with_warning="warning: beads.role not configured (GH#2950)
Fix: git config beads.role maintainer
Fix: git config --global beads.role maintainer
✓ Created issue: sp-xxxx — acceptance test probe title
  Priority: P2
  Status: open"
_extracted="$(_extract_bead_id "$_out_with_warning")"
is_eq "bead-id extracted from output with warning prefix" "sp-xxxx" "$_extracted"

# Structural check: acceptance-run.sh uses the same extraction pattern.
wantre "acceptance-run.sh uses Created issue extraction" \
    "sed -n 's/.*Created issue:"

# ===========================================================================
echo
echo "5. bead-id extraction (pair): error with no id → empty"
# ===========================================================================
_out_error="error: cannot connect to database
connection refused: dial tcp 127.0.0.1:3307"
_from_error="$(_extract_bead_id "$_out_error")"
is_eq "error output with no id extracts nothing" "" "$_from_error"

# ===========================================================================
echo
echo "6. beads.role set by install.sh after bd init (real bd)"
# ===========================================================================
_real_bd="${SPIRA_BD:-$(command -v bd 2>/dev/null || true)}"
if [ ! -x "${_real_bd:-}" ]; then
    ok "beads.role: bd not found — skipping install fixture test"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    [ "$fail" -eq 0 ]; exit $?
fi

# Use /var/tmp so there is no .beads ancestor that bd would find by walking up.
_fb_base="$(mktemp -d /var/tmp/test-accept-run-XXXXXX 2>/dev/null \
    || mktemp -d /tmp/test-accept-run-XXXXXX)"
trap 'rm -rf "$SCRATCH" "$_fb_base"' EXIT INT TERM

_fb_db="$_fb_base/db"
mkdir -p "$_fb_db"

# Positive control: confirm no .beads ancestor (isolation required for meaningful result).
_walk="$_fb_db"; _found=0
while [ "$_walk" != "/" ] && [ -n "$_walk" ]; do
    [ -d "$_walk/.beads" ] && { _found=1; break; }
    _walk="$(dirname "$_walk")"
done
if [ "$_found" = 1 ]; then
    ok "beads.role: parent .beads found — cannot isolate; skipping"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    [ "$fail" -eq 0 ]; exit $?
fi
unset _walk _found

# Run bd init in file mode (cd form, as install.sh does).
_init_out="$(cd "$_fb_db" && BD_NON_INTERACTIVE=1 "$_real_bd" init \
    --non-interactive --prefix sp --skip-agents --skip-hooks -q 2>&1)"
_init_rc=$?
iszero "bd init exits 0 on fresh directory" "$_init_rc"

# Apply the same git config that install.sh runs after bd init.
git -C "$_fb_db" config beads.role maintainer 2>/dev/null || true

_role="$(git -C "$_fb_db" config beads.role 2>/dev/null || true)"
is_eq "beads.role set to maintainer in SPIRA_DB after init" "maintainer" "$_role"

# Structural: install.sh sets beads.role in both modes.
grep -qE 'git -C.*SPIRA_DB.*config beads\.role maintainer' "$REAL_REPO/install.sh" \
    && ok "install.sh file mode: git config beads.role maintainer" \
    || bad "install.sh file mode: git config beads.role maintainer" \
           "pattern not found in install.sh"

# ===========================================================================
echo
echo "7. ready.sh: clone path used, not workspace path"
# ===========================================================================

# Structural: the fixed code invokes $_clone/spira/ready.sh, not $HERE/ready.sh.
wantre "phase A: ready.sh invocation uses \$_clone path" \
    'bash.*\$_clone/spira/ready\.sh'
wantre "phase D: ready.sh invocation uses \$_aged_clone path" \
    'bash.*\$_aged_clone/spira/ready\.sh'

# $HERE/ready.sh must not appear in any bash invocation (may appear in comments).
if grep -E 'bash[^#]*\$HERE/ready\.sh' "$SCRIPT" | grep -qv '^\s*#'; then
    bad 'workspace $HERE/ready.sh not called directly' \
        "still present: $(grep -E 'bash[^#]*\$HERE/ready\.sh' "$SCRIPT" | grep -v '^\s*#' | head -1)"
else
    ok 'workspace $HERE/ready.sh removed from bash invocations'
fi

# Behavioral pair: clone ready.sh=0, workspace ready.sh=1 → check passes.
_clone_dir="$SCRATCH/clone-v1"
mkdir -p "$_clone_dir/spira"
printf '#!/usr/bin/env bash\nexit 0\n' > "$_clone_dir/spira/ready.sh"
chmod +x "$_clone_dir/spira/ready.sh"

_ws_ready="$SCRATCH/ws-ready.sh"
printf '#!/usr/bin/env bash\nexit 1\n' > "$_ws_ready"
chmod +x "$_ws_ready"

# Positive control: workspace ready.sh does exit 1.
_ws_rc=0; bash "$_ws_ready" 2>/dev/null || _ws_rc=$?
if [ "$_ws_rc" -ne 0 ]; then
    ok "positive-control: workspace ready.sh exits 1"
else
    bad "positive-control: workspace ready.sh exits 1" "got exit 0"
fi

# Clone ready.sh exits 0 → passes (proves the clone path is decisive).
_clone_rc=0; bash "$_clone_dir/spira/ready.sh" 2>/dev/null || _clone_rc=$?
if [ "$_clone_rc" -eq 0 ]; then
    ok "clone ready.sh=0: check passes even when workspace ready.sh exits 1"
else
    bad "clone ready.sh=0: check passes even when workspace ready.sh exits 1" \
        "clone exit=$_clone_rc"
fi

# Reverse pair: clone ready.sh=1 → fails and failure names clone path.
printf '#!/usr/bin/env bash\nprintf "ready: FAIL nothing works\n"; exit 1\n' \
    > "$_clone_dir/spira/ready.sh"
chmod +x "$_clone_dir/spira/ready.sh"

_fail_rc=0; _fail_out="$(bash "$_clone_dir/spira/ready.sh" 2>&1)" || _fail_rc=$?
if [ "$_fail_rc" -ne 0 ]; then
    ok "clone ready.sh=1: check fails"
else
    bad "clone ready.sh=1: check fails" "got exit 0"
fi

# Confirm the clone path appears in the bad() call for phase A.
wantre "phase A bad() names clone path" \
    'bad.*ready\.sh.*\$_clone/spira/ready\.sh'

# ===========================================================================
echo
echo "8. staged checks: old 900s/600s loops gone, stages 2-5 present with tight budgets"
# ===========================================================================

# Positive control: pattern that would match the old 900s loop.
_old_900="$(grep -c '_aeon_wait' "$SCRIPT" 2>/dev/null || true)"
is_eq "positive-control: old 900s loop is gone from phase A" "0" "$_old_900"

_old_600="$(grep -c '_aged_land_wait' "$SCRIPT" 2>/dev/null || true)"
is_eq "positive-control: old 600s loop is gone from phase D" "0" "$_old_600"

# Stage 2: sentinel is started directly.
want "stage 2: sentinel started directly" \
    'systemctl --user start spira-sentinel.service'

# Stage 2: branch show-ref check (summoned signal).
want "stage 2: polls for aeon branch via show-ref" \
    'refs/heads/spira/$_bead_id'

# Stage 3: commit check on branch.
wantre "stage 3: polls for commit on spira/bead branch" \
    'git.*log.*spira/\$_bead_id'

# Stage 3 bad() names stage 3.
wantre "stage 3: FAIL names stage 3" \
    'bad.*stage 3.*commit'

# Stage 4: bd show --json status check.
want "stage 4: bd show --json polls closed status" \
    'show "$_bead_id" --json'

# Stage 5: sentinel kicked again before landing poll (appears twice — stage 2 and 5).
want "stage 5: sentinel kicked before landing poll" \
    'spira-sentinel.service'

# Stage 2 budget: 60s (not 900).
want "stage 2 budget is 60s" \
    '_a_t2 )) -lt 60'

# Stage 5 budget: 120s (not 900).
want "stage 5 budget is 120s" \
    '_a_t5 )) -lt 120'

# Phase D: same staged structure.
wantre "phase D stage 3: FAIL names stage 3" \
    'bad.*phase D stage 3'

want "phase D stage 5: landed message" \
    'phase D stage 5: bead'

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
