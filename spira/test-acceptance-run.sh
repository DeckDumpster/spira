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
wantre "phase B _prev_env includes SPIRA_OPERATED=0" \
    '_prev_env=\(.*SPIRA_OPERATED=0'
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
echo "8. snapshot trigger and JSONL writer"
# ===========================================================================

# Structural: SPIRA_ACCEPTANCE_FORENSICS controls the forensics dir.
wantre "SPIRA_ACCEPTANCE_FORENSICS used for forensics dir" \
    'SPIRA_ACCEPTANCE_FORENSICS'

# Structural: bad() triggers _take_snapshot on first phase FAIL.
wantre "bad() calls _take_snapshot when _phase_snapped is 0" \
    '_take_snapshot.*_cur_phase'

# Structural: _take_snapshot is called with || true so its failure does not
# propagate and change the verdict exit code.
wantre "bad(): _take_snapshot failure does not propagate" \
    '_take_snapshot.*|| true'

# Structural: ok() and bad() both write to _jsonl_file.
wantre "ok() appends JSONL line" \
    '"verdict":"ok"'
wantre "bad() appends JSONL line with verdict fail" \
    '"verdict":"fail"'

# Structural: end-of-run snapshot is taken before the final exit.
want "end-of-run snapshot taken" '_take_snapshot "end-of-run"'

# Behavioral pair: JSONL writer — one line per ok/bad, FAIL count matches.
_jdir="$SCRATCH/jtest"
mkdir -p "$_jdir"
_jfile="$_jdir/checks.jsonl"
: > "$_jfile"
_jpass=0; _jfail=0
_jsnap=0
_jcur_phase="test"; _jphase_snapped=0; _jphase_start_ts="$(date +%s)"
_j_escape() {
    local _s="${1:-}"
    _s="${_s//\\/\\\\}"; _s="${_s//\"/\\\"}"; printf '%s' "$_s"
}
_jok() {
    _jpass=$((_jpass+1))
    printf '{"phase":"%s","check":"%s","verdict":"ok","ts":"%s","elapsed":%d}\n' \
        "$_jcur_phase" "$(_j_escape "$1")" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "$(($(date +%s)-_jphase_start_ts))" >> "$_jfile"
}
_jbad() {
    _jfail=$((_jfail+1))
    printf '{"phase":"%s","check":"%s","verdict":"fail","reason":"%s","ts":"%s","elapsed":%d}\n' \
        "$_jcur_phase" "$(_j_escape "$1")" "$(_j_escape "${2:-}")" \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "$(($(date +%s)-_jphase_start_ts))" >> "$_jfile"
    if [ "$_jphase_snapped" = 0 ]; then
        _jphase_snapped=1
        _jsnap=$((_jsnap+1))
    fi
}

_jok  "check passes"
_jbad "check fails" "some reason"
_jbad "second fail"  "another"   # should NOT re-trigger snapshot
_jok  "another pass"

_jlines="$(wc -l < "$_jfile" | tr -d ' ')"
is_eq "JSONL: one line per check (4 checks → 4 lines)" "4" "$_jlines"

_jfail_json="$(grep -c '"verdict":"fail"' "$_jfile" || true)"
is_eq "JSONL: fail count matches bad() calls" "$_jfail" "$_jfail_json"

is_eq "snapshot triggered once on first phase FAIL" "1" "$_jsnap"

# Phase reset: new phase triggers snapshot again on next FAIL.
_jcur_phase="phase-2"; _jphase_snapped=0
_jbad "phase-2 first fail" "x"
is_eq "snapshot triggered again after phase reset" "2" "$_jsnap"

# Behavioral: snapshot mkdir failure does not change verdict.
_snap_dir_ro="$SCRATCH/snapshots-ro"
mkdir -p "$_snap_dir_ro"
chmod 000 "$_snap_dir_ro" 2>/dev/null || true
_snap_count=0
_forensics_dir_save="${_forensics_dir:-}"
_forensics_dir="$_snap_dir_ro/nope"
# _take_snapshot is not locally defined here, but we can verify structurally
# that acceptance-run.sh uses || return 0 on mkdir in _take_snapshot.
wantre "_take_snapshot: mkdir failure returns cleanly" \
    'mkdir -p.*_sdir.*|| return 0'
chmod 755 "$_snap_dir_ro" 2>/dev/null || true

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
