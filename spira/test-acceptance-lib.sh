#!/usr/bin/env bash
# tier: T1
# covers: spira/acceptance-lib.sh UC-instance-lifecycle-47
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
LIB="$HERE/acceptance-lib.sh"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

echo "test-acceptance-lib.sh"

# Every acceptance-lib.sh function is exercised in a child `bash -c`, never sourced
# into this suite's own shell: acceptance-lib.sh defines ok/bad/want/is0, the same
# names testlib.sh defines with a different contract, so sourcing it here would
# silently replace this suite's own assertion functions.

echo
echo "1. POSITIVE CONTROL — acceptance-lib.sh exists and is sourceable"

if [ -f "$LIB" ] && bash -c '. "$0"' "$LIB" 2>/dev/null; then
    ok "acceptance-lib.sh exists and sources cleanly"
else
    bad "acceptance-lib.sh exists and sources cleanly" "missing or errors on source"
    tl_summary
fi

# ===========================================================================
echo
echo "2. _extract_bead_id: GH#2950 warning prefix, and the empty pairs either side"
# ===========================================================================

_lib_extract() { bash -c '. "$0"; _extract_bead_id "$1"' "$LIB" "$1"; }

is "positive-control: no id in warning-only output -> empty" \
    "" "$(_lib_extract "warning: beads.role not configured (GH#2950)")"

_out_with_warning="warning: beads.role not configured (GH#2950)
Fix: git config beads.role maintainer
Fix: git config --global beads.role maintainer
✓ Created issue: sp-xxxx — acceptance test probe title
  Priority: P2
  Status: open"
is "bead id extracted past a warning prefix" \
    "sp-xxxx" "$(_lib_extract "$_out_with_warning")"

is "pair: error output with no id extracts nothing" \
    "" "$(_lib_extract "error: cannot connect to database
connection refused: dial tcp 127.0.0.1:3307")"

# ===========================================================================
echo
echo "3. _phase_env: SPIRA_OPERATED=0 always present; SPIRA_AGENT only when given"
# ===========================================================================
# Pinned to non-default values throughout: a literal default would pass even if
# _phase_env stopped forwarding its arguments.

_lib_phase_env() {  # <conf> <releases> <home-repo> [agent]
    bash -c '. "$0"; _phase_env _pe "$1" "$2" "$3" "${4:-}"; printf "%s\n" "${_pe[@]}"' \
        "$LIB" "$1" "$2" "$3" "${4:-}"
}

_pe_out="$(_lib_phase_env "/tmp/pinned-conf" "/tmp/pinned-releases" "pinned-repo")"
want "no agent: SPIRA_OPERATED=0 present"     "SPIRA_OPERATED=0"                  "$_pe_out"
want "no agent: conf forwarded"               "SPIRA_CONF=/tmp/pinned-conf"       "$_pe_out"
want "no agent: releases forwarded"           "SPIRA_RELEASES=/tmp/pinned-releases" "$_pe_out"
want "no agent: home-repo forwarded"          "SPIRA_HOME_REPO=pinned-repo"       "$_pe_out"
nowant "no agent: SPIRA_AGENT absent"         "SPIRA_AGENT"                       "$_pe_out"

_pe_agent_out="$(_lib_phase_env "/tmp/pinned-conf" "/tmp/pinned-releases" "pinned-repo" "/tmp/pinned-agent")"
want "with agent: SPIRA_OPERATED=0 present"   "SPIRA_OPERATED=0"                  "$_pe_agent_out"
want "with agent: SPIRA_AGENT forwarded"      "SPIRA_AGENT=/tmp/pinned-agent"     "$_pe_agent_out"

# ===========================================================================
echo
echo "4. _run_ready: exit code is read from this call's own return, never a stale \$?"
# ===========================================================================
# POSITIVE CONTROL first: the bug this replaces was `out=\$(cmd) || true; rc=\$?`,
# which always reports 0 because \$? after \`|| true\` names true's exit, not cmd's
# (law-status-after-a-pipe-is-the-last-command).
_old_out="$(exit 3)" || true
_old_rc_after=$?
is "positive-control: the old || true pattern always yields rc=0" "0" "$_old_rc_after"

_lib_run_ready() { bash -c '. "$0"; _run_ready "$@"' "$LIB" "$@"; }

STUB_FAIL="$SCRATCH/ready-fail.sh"
printf '#!/bin/sh\nprintf "stub FAIL output\\n"\nexit 3\n' > "$STUB_FAIL"
chmod +x "$STUB_FAIL"

_rr_out="$(_lib_run_ready "$STUB_FAIL")"; _rr_rc=$?
is "exit 3 is captured, not swallowed"       "3"                  "$_rr_rc"
want "failing output is still returned"      "stub FAIL output"   "$_rr_out"

STUB_OK="$SCRATCH/ready-ok.sh"
printf '#!/bin/sh\nexit 0\n' > "$STUB_OK"
chmod +x "$STUB_OK"

_rr_out2="$(_lib_run_ready "$STUB_OK")"; _rr_rc2=$?
is "exit 0 is captured" "0" "$_rr_rc2"

STUB_ENV="$SCRATCH/ready-env.sh"
cat > "$STUB_ENV" <<'EOF'
#!/bin/sh
printf 'OPERATED=%s\n' "$SPIRA_OPERATED"
exit 0
EOF
chmod +x "$STUB_ENV"
want "env pairs are forwarded to the ready script" \
    "OPERATED=0" "$(_lib_run_ready "$STUB_ENV" SPIRA_OPERATED=0)"

# ===========================================================================
echo
echo "5. _check_release_bins: names what is missing, empty when nothing is"
# ===========================================================================

_lib_check_bins() { bash -c '. "$0"; _check_release_bins "$1"' "$LIB" "$1"; }

_rel_missing="$SCRATCH/rel-missing/bin"
mkdir -p "$_rel_missing"
for _b in loom panel broker; do
    printf '#!/bin/sh\n' > "$_rel_missing/$_b" && chmod +x "$_rel_missing/$_b"
done
# spira-supervise intentionally absent — positive control.
want "positive-control: names the missing binary" \
    "spira-supervise" "$(_lib_check_bins "$(dirname "$_rel_missing")")"

_rel_full="$SCRATCH/rel-full/bin"
mkdir -p "$_rel_full"
for _b in loom panel broker spira-supervise landing-pass; do
    printf '#!/bin/sh\n' > "$_rel_full/$_b" && chmod +x "$_rel_full/$_b"
done
is "pair: nothing missing -> empty" "" "$(_lib_check_bins "$(dirname "$_rel_full")")"

# ===========================================================================
echo
echo "6. _install_from_tarball: forwards conf/releases/force to activate.sh, propagates its exit"
# ===========================================================================

FAKEHERE="$SCRATCH/fakehere"
mkdir -p "$FAKEHERE"
cat > "$FAKEHERE/activate.sh" <<'EOF'
#!/bin/sh
printf 'activate: tb=%s conf=%s releases=%s force=%s\n' \
    "$1" "$SPIRA_CONF" "$SPIRA_RELEASES" "$SPIRA_ACTIVATE_FORCE"
exit "${ACTIVATE_EXIT:-0}"
EOF
chmod +x "$FAKEHERE/activate.sh"

_lib_install_from_tarball() {  # <tarball> <releases-dir> <conf>
    bash -c '. "$0"; HERE="$1"; _install_from_tarball "$2" "$3" "$4"' \
        "$LIB" "$FAKEHERE" "$1" "$2" "$3"
}

_ift_out="$(_lib_install_from_tarball "/tmp/pinned.tar.gz" "/tmp/pinned-rel" "/tmp/pinned-conf")"
_ift_rc=$?
is "activate.sh success -> _install_from_tarball exits 0" "0" "$_ift_rc"
want "tarball path forwarded"   "tb=/tmp/pinned.tar.gz"      "$_ift_out"
want "conf forwarded"           "conf=/tmp/pinned-conf"      "$_ift_out"
want "releases forwarded"       "releases=/tmp/pinned-rel"   "$_ift_out"
want "SPIRA_ACTIVATE_FORCE=1"   "force=1"                    "$_ift_out"

ACTIVATE_EXIT=1 bash -c '. "$0"; HERE="$1"; _install_from_tarball "$2" "$3" "$4"' \
    "$LIB" "$FAKEHERE" "/tmp/x.tar.gz" "/tmp/rel" "/tmp/conf" >/dev/null 2>&1
_ift_fail_rc=$?
is "activate.sh failure -> _install_from_tarball returns non-zero" "1" "$_ift_fail_rc"

# ===========================================================================
echo
echo "7. checks framework: ok/bad/is0/not0/want/notwant/is_same tally correctly"
# ===========================================================================
# The old suite reimplemented ok()/bad() by hand to test the JSONL shape; that
# copy could pass while the real acceptance-run.sh implementation drifted. This
# drives the real functions instead.

_checks_out="$(bash -c '
    . "$0"
    _jsonl_file=/dev/null
    _snap_count=0
    is0     "a" 0
    is0     "b" 1
    not0    "c" 1
    not0    "d" 0
    want    "e" needle "a needle b"
    want    "f" needle "no match"
    notwant "g" needle "no match"
    is_same "h" x x
    is_same "i" x y
    printf "pass=%s fail=%s\n" "$pass" "$fail"
' "$LIB")"
want "is0/not0/want/notwant/is_same: 5 pass, 4 fail" "pass=5 fail=4" "$_checks_out"

# ===========================================================================
echo
echo "8. checks framework: JSONL — one line per check, fail count matches, snapshot once per phase"
# ===========================================================================

_jfile="$SCRATCH/checks.jsonl"
_fw_out="$(bash -c '
    . "$0"
    _jsonl_file="$1"
    _cur_phase="test"; _phase_start_ts=$(date +%s); _phase_snapped=0; _snap_count=0
    : > "$_jsonl_file"
    ok  "check passes"
    bad "check fails" "some reason"
    bad "second fail" "another"
    ok  "another pass"
    _cur_phase="phase-2"; _phase_snapped=0
    bad "phase-2 first fail" "x"
    printf "pass=%s fail=%s snap=%s\n" "$pass" "$fail" "$_snap_count"
' "$LIB" "$_jfile")"
want "framework tally: 2 pass, 3 fail" "pass=2 fail=3 snap=2" "$_fw_out"

_jlines="$(wc -l < "$_jfile" | tr -d ' ')"
is "JSONL: one line per check (5 checks -> 5 lines)" "5" "$_jlines"

_jfail_json="$(grep -c '"verdict":"fail"' "$_jfile" || true)"
is "JSONL: fail count matches bad() calls" "3" "$_jfail_json"

# ===========================================================================
echo
echo "9. _take_snapshot: mkdir failure returns cleanly; a writable dir gets one"
# ===========================================================================

_ro_parent="$SCRATCH/snap-ro"
mkdir -p "$_ro_parent"
chmod 000 "$_ro_parent" 2>/dev/null || true
bash -c '
    . "$0"
    _forensics_dir="$1"
    _snap_count=0
    _take_snapshot "unwritable" >/dev/null 2>&1
' "$LIB" "$_ro_parent/nope"
wantrc "mkdir failure inside _take_snapshot does not propagate" "0" "$?"
chmod 755 "$_ro_parent" 2>/dev/null || true

_rw_dir="$SCRATCH/snap-rw"
mkdir -p "$_rw_dir"
bash -c '
    . "$0"
    _forensics_dir="$1"
    _snap_count=0
    _run_start_wall="today"
    _take_snapshot "unit-test" >/dev/null 2>&1
' "$LIB" "$_rw_dir"
[ -d "$_rw_dir/01-unit-test" ] \
    && ok "a writable forensics dir gets a numbered snapshot directory" \
    || bad "a writable forensics dir gets a numbered snapshot directory" \
           "$_rw_dir/01-unit-test not created"

# ===========================================================================
echo
echo "10. _acquire_tarball: --tarball skips the download; gh is never invoked"
# ===========================================================================
# POSITIVE CONTROL FIRST: prove a poisoned gh WOULD be reached on the download
# path, before trusting its silence on the --tarball path.

_lib_acquire() { bash -c '. "$0"; _acquire_tarball "$1" "$2" "$3"' "$LIB" "$1" "$2" "$3"; }

POISON_BIN="$SCRATCH/poison-bin"
mkdir -p "$POISON_BIN"
POISON_LOG="$SCRATCH/gh-invoked"
cat > "$POISON_BIN/gh" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >> "$POISON_LOG"
exit 1
EOF
chmod +x "$POISON_BIN/gh"

rm -f "$POISON_LOG"
_dl_dir="$SCRATCH/dl-empty-given"
mkdir -p "$_dl_dir"
PATH="$POISON_BIN:$PATH" _lib_acquire "some-tag" "" "$_dl_dir" >/dev/null 2>&1
want "positive-control: empty --tarball reaches gh (download path taken)" \
    "release download some-tag" "$(cat "$POISON_LOG" 2>/dev/null || true)"

rm -f "$POISON_LOG"
GIVEN_TB="$SCRATCH/given.tar.gz"
printf 'fake tarball\n' > "$GIVEN_TB"
_agiven_out="$(PATH="$POISON_BIN:$PATH" _lib_acquire "some-tag" "$GIVEN_TB" "$_dl_dir")"
_agiven_rc=$?
is "given path present -> exits 0" "0" "$_agiven_rc"
is "given path present -> prints the given path, unchanged" "$GIVEN_TB" "$_agiven_out"
[ -f "$POISON_LOG" ] \
    && bad "given path present -> gh is never invoked" "gh was called: $(cat "$POISON_LOG")" \
    || ok "given path present -> gh is never invoked"

_amiss_out="$(_lib_acquire "some-tag" "$SCRATCH/does-not-exist.tar.gz" "$_dl_dir")"
_amiss_rc=$?
is "pair: given path absent -> non-zero, nothing printed" "1:" "$_amiss_rc:$_amiss_out"

tl_summary
