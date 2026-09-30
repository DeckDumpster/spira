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
    PATH="$FAKEHERE:$PATH" bash -c '. "$0"; HERE="$1"; _install_from_tarball "$2" "$3" "$4"' \
        "$LIB" "$FAKEHERE" "$1" "$2" "$3"
}

_ift_out="$(_lib_install_from_tarball "/tmp/pinned.tar.gz" "/tmp/pinned-rel" "/tmp/pinned-conf")"
_ift_rc=$?
is "activate.sh success -> _install_from_tarball exits 0" "0" "$_ift_rc"
want "tarball path forwarded"   "tb=/tmp/pinned.tar.gz"      "$_ift_out"
want "conf forwarded"           "conf=/tmp/pinned-conf"      "$_ift_out"
want "releases forwarded"       "releases=/tmp/pinned-rel"   "$_ift_out"
want "SPIRA_ACTIVATE_FORCE=1"   "force=1"                    "$_ift_out"

ACTIVATE_EXIT=1 PATH="$FAKEHERE:$PATH" bash -c '. "$0"; HERE="$1"; _install_from_tarball "$2" "$3" "$4"' \
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

# ===========================================================================
echo
echo "14. _check_oneshots: starts oneshot units only, and surfaces a failed start"
# ===========================================================================
# POSITIVE CONTROL: spira-beta.service's start is made to fail, so a silent pass below
# is trusted (law-absence-needs-a-positive-control). spira-gamma.service is Type=simple
# and must never be started at all.

_co_bin="$SCRATCH/co-bin"; mkdir -p "$_co_bin"
_co_log="$SCRATCH/co-started"
rm -f "$_co_log"
cat > "$_co_bin/systemctl" <<'SC'
#!/usr/bin/env bash
case "$2" in
    list-unit-files)
        printf 'spira-alpha.service enabled\nspira-beta.service enabled\nspira-gamma.service enabled\n'
        ;;
    show)
        case "${*: -1}" in
            spira-gamma.service) printf 'simple\n' ;;
            *)                   printf 'oneshot\n' ;;
        esac
        ;;
    start)
        printf '%s\n' "${*: -1}" >> "$CO_LOG"
        [ "${*: -1}" = "spira-beta.service" ] && exit 1
        exit 0
        ;;
    *) exit 0 ;;
esac
SC
chmod +x "$_co_bin/systemctl"

_co_out="$(PATH="$_co_bin:$PATH" CO_LOG="$_co_log" bash -c '
    . "$0"
    _cur_phase="test"; _phase_start_ts=0; _jsonl_file=/dev/null
    _check_oneshots "phase X"
' "$LIB" 2>&1)"

want "a failed oneshot start is surfaced, naming the unit" \
    "failed: spira-beta.service" "$_co_out"
want "the succeeding oneshot unit was started" "spira-alpha.service" "$(cat "$_co_log")"
nowant "positive-control: the non-oneshot unit is never started" \
    "spira-gamma.service" "$(cat "$_co_log")"

# ===========================================================================
echo
echo "15. _take_snapshot: passes --plain to list-units (no bullet-prefixed unit names)"
# ===========================================================================
# Without --plain, systemctl prefixes a failed unit's line with "● ", and awk '{print $1}'
# takes the bullet as the unit name — the forensics collector then wrote the failed unit's
# own status/journal to a file named literally "●.txt" instead of capturing it at all.

_ts_bin="$SCRATCH/ts-bin"; mkdir -p "$_ts_bin"
cat > "$_ts_bin/systemctl" <<'SC'
#!/usr/bin/env bash
case "$2" in
    list-units)
        _plain=0
        for _a in "$@"; do [ "$_a" = "--plain" ] && _plain=1; done
        if [ "$_plain" = 1 ]; then
            printf 'spira-broken.service loaded failed failed X\n'
        else
            printf '\xe2\x97\x8f spira-broken.service loaded failed failed X\n'
        fi
        ;;
    list-timers) printf '' ;;
    status)      printf 'status-ok\n' ;;
    *)           exit 0 ;;
esac
SC
chmod +x "$_ts_bin/systemctl"
cat > "$_ts_bin/journalctl" <<'SC'
#!/usr/bin/env bash
printf 'journal-ok\n'
SC
chmod +x "$_ts_bin/journalctl"

_snap2_dir="$SCRATCH/snap2"; mkdir -p "$_snap2_dir"
PATH="$_ts_bin:$PATH" bash -c '
    . "$0"
    _forensics_dir="$1"
    _snap_count=0
    _run_start_wall="today"
    _take_snapshot "unit-test" >/dev/null 2>&1
' "$LIB" "$_snap2_dir"

[ -f "$_snap2_dir/01-unit-test/journal/spira-broken.service.txt" ] \
    && ok "journal captured under the real unit name" \
    || bad "journal captured under the real unit name" \
           "$(ls "$_snap2_dir/01-unit-test/journal" 2>&1)"
[ -e "$_snap2_dir/01-unit-test/journal/●.txt" ] \
    && bad "positive-control: no bullet-named journal file" "●.txt was written" \
    || ok "positive-control: no bullet-named journal file"

_amiss_out="$(_lib_acquire "some-tag" "$SCRATCH/does-not-exist.tar.gz" "$_dl_dir")"
_amiss_rc=$?
is "pair: given path absent -> non-zero, nothing printed" "1:" "$_amiss_rc:$_amiss_out"

# ===========================================================================
echo
echo "11. _bead_finished: closed, or submitted (sp-qsona) — the aeon's close either way"
# ===========================================================================
# Under sp-qsona an aeon's close of a work bead becomes open + spira-submitted and only the
# landing pass closes it, so stage 4 ("the aeon closed it") polled for "closed" and failed
# whenever landing took longer than its 30s window (local phase D, 2026-09-26).
_bf_bd="$SCRATCH/bd-bf"
cat > "$_bf_bd" <<'BD'
#!/usr/bin/env bash
case "${BF_CASE:-}" in
    closed)    printf '[{"id":"x","status":"closed","labels":[]}]\n' ;;
    submitted) printf '[{"id":"x","status":"open","labels":["spira","spira-submitted"]}]\n' ;;
    open)      printf '[{"id":"x","status":"open","labels":["spira"]}]\n' ;;
    *)         exit 1 ;;
esac
BD
chmod +x "$_bf_bd"
_lib_bf() { PATH="$SCRATCH/bfbin:$PATH" BF_CASE="$1" bash -c '. "$0"; _bead_finished db x' "$LIB"; }
mkdir -p "$SCRATCH/bfbin"; cp "$_bf_bd" "$SCRATCH/bfbin/bd"
_lib_bf closed;    wantrc "closed -> finished"                       0 $?
_lib_bf submitted; wantrc "open + spira-submitted -> finished"       0 $?
_lib_bf open;      wantrc "positive control: plain open -> not finished" 1 $?
_lib_bf broken;    wantrc "unreadable -> not finished"               1 $?

# ===========================================================================
echo
echo "12. _unit_set: installed spira-* unit files, transient units excluded"
# ===========================================================================
# A systemd-run transient (the landing pass's spira-landing.service) that happens to be alive
# when one snapshot is taken is not part of the installed unit set; counting it made phase C's
# "unit set after rollback matches" differ on timing alone ("19a20 > spira-landing.service
# transient", 2026-09-26).
_us_sc="$SCRATCH/usbin"; mkdir -p "$_us_sc"
cat > "$_us_sc/systemctl" <<'SC'
#!/usr/bin/env bash
printf 'spira-b-prod.service enabled enabled\nspira-landing.service transient -\nspira-a-prod.timer enabled enabled\nother.service enabled enabled\n'
SC
chmod +x "$_us_sc/systemctl"
is "only installed spira-* units, sorted, transient dropped" \
   "$(printf 'spira-a-prod.timer enabled\nspira-b-prod.service enabled')" \
   "$(PATH="$_us_sc:$PATH" bash -c '. "$0"; _unit_set' "$LIB")"

# ===========================================================================
echo
echo "13. _stage_release_source: a release tarball + its tag, as skew reads a local source"
# ===========================================================================
# The acceptance box's user units have no forge credential; the releases the run itself
# downloads or builds are staged here, and spira.conf points SPIRA_RELEASE_REPO at it.
_rs_dir="$SCRATCH/release-source"
printf 'x\n' > "$SCRATCH/spira-20990101T000000Z.tar.gz"
bash -c '. "$0"; _stage_release_source "$1" "$2" "$3"' "$LIB" \
    "$_rs_dir" "$SCRATCH/spira-20990101T000000Z.tar.gz" spira-release-spira-20990101T000000Z
wantrc "staging exits 0" 0 $?
[ -f "$_rs_dir/spira-20990101T000000Z.tar.gz" ] \
    && ok  "the tarball is in the source directory" \
    || bad "the tarball is in the source directory" "$(ls "$_rs_dir" 2>&1)"
is "its .tag sidecar names the release tag" "spira-release-spira-20990101T000000Z" \
   "$(cat "$_rs_dir/spira-20990101T000000Z.tag" 2>/dev/null)"
bash -c '. "$0"; _stage_release_source "$1" "$2" "$3"' "$LIB" \
    "$_rs_dir" "$SCRATCH/absent.tar.gz" spira-release-spira-20990101T000001Z
wantrc "a missing tarball is refused" 1 $?

# ===========================================================================
echo
echo "16. _ci_env: PATH puts the RELEASE's bin/ first (spira-config and every tool by name)"
# ===========================================================================
# sp-seae6: deploy.sh/uninstall.sh/world.sh/doctor.sh run straight from this checkout
# (never an activated release), and every tool they call — spira-config, without which no
# config (SPIRA_OPERATED=0 included) is ever read — is invoked by bare name (sp-gypjk), so
# _ci_env is their launcher and sets PATH. Pinned to non-default paths: a literal default would pass
# even if _ci_env stopped forwarding its argument or _releases stopped being read.
_lib_ci_env() {  # <conf>
    bash -c '_releases="/tmp/pinned-releases"; . "$0"; _ci_env _ce "$1"; printf "%s\n" "${_ce[@]}"' \
        "$LIB" "$1"
}

_ce_out="$(_lib_ci_env "/tmp/pinned-conf")"
want "conf forwarded"          "SPIRA_CONF=/tmp/pinned-conf"                              "$_ce_out"
want "PATH leads with the release's bin/, not this checkout's" \
    "PATH=/tmp/pinned-releases/current/bin:"                                               "$_ce_out"

# ===========================================================================
echo
echo "17. _wait_for_release_asset: bounded poll for the tarball asset (sp-5olmi)"
# ===========================================================================
# POSITIVE CONTROL FIRST: a tag whose asset never appears must time out, not hang —
# this is the exact scenario the current (unpatched) ordering gets wrong: acceptance
# fires the instant the tag lands and phase A's one-shot download fails immediately
# because release.yml has not uploaded the tarball yet. Under a short timeout this
# proves the wait is bounded rather than looping forever.

_wa_bin="$SCRATCH/wa-bin"; mkdir -p "$_wa_bin"
_wa_calls="$SCRATCH/wa-calls"

cat > "$_wa_bin/gh" <<'GH'
#!/usr/bin/env bash
n=0
[ -f "$WA_CALLS" ] && n=$(wc -l < "$WA_CALLS")
printf 'call\n' >> "$WA_CALLS"
if [ "$n" -ge "${WA_READY_AFTER:-999999}" ]; then
    printf 'spira-20990101T000000Z.tar.gz\n'
fi
GH
chmod +x "$_wa_bin/gh"

rm -f "$_wa_calls"
PATH="$_wa_bin:$PATH" WA_CALLS="$_wa_calls" WA_READY_AFTER=999999 \
    bash -c '. "$0"; _wait_for_release_asset "tag-before-asset" 1 0.2' "$LIB"
wantrc "positive-control: asset that never appears times out (bounded, not a hang)" \
    1 $?
_wa_timeout_calls="$(wc -l < "$_wa_calls" | tr -d ' ')"
[ "${_wa_timeout_calls:-0}" -gt 1 ] \
    && ok "positive-control: gh was actually polled more than once before timing out" \
    || bad "positive-control: gh was actually polled more than once before timing out" \
           "calls=$_wa_timeout_calls"

# Now believe the silence: the same race, but release.yml's upload catches up
# after a couple of polls — the fix must wait it out and then succeed.
rm -f "$_wa_calls"
PATH="$_wa_bin:$PATH" WA_CALLS="$_wa_calls" WA_READY_AFTER=2 \
    bash -c '. "$0"; _wait_for_release_asset "tag-race" 30 0.1' "$LIB"
wantrc "asset appears after the upload catches up -> returns 0" 0 $?

# _ar_gh_repo forwarded to gh as --repo, the same as _download_tarball.
_wa_repo_log="$SCRATCH/wa-repo-log"
cat > "$_wa_bin/gh" <<'GH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$WA_REPO_LOG"
printf 'spira-20990101T000000Z.tar.gz\n'
GH
chmod +x "$_wa_bin/gh"
rm -f "$_wa_repo_log"
PATH="$_wa_bin:$PATH" WA_REPO_LOG="$_wa_repo_log" \
    bash -c '_ar_gh_repo="owner/repo"; . "$0"; _wait_for_release_asset "tag-repo" 5' "$LIB" >/dev/null
want "the configured repo is forwarded via --repo" \
    "--repo owner/repo release view tag-repo" "$(cat "$_wa_repo_log" 2>/dev/null || true)"

tl_summary
