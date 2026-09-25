#!/usr/bin/env bash
# acceptance-run.sh — release acceptance: fresh install from the release tarball,
# land a bead end to end, uninstall; optionally also upgrade and rollback paths.
#
# Run this on a CLEAN machine (no existing Spira installation, no conflicting systemd units).
#
# Usage:
#   acceptance-run.sh <tag> --scratch-repo <path> [--prev-tag <tag>] [--record]
#                           [--file-defects] [--bd-db <path>] [--agent <path>]
#
# Arguments:
#   <tag>                  release tag to test (spira-release-spira-*)
#   --scratch-repo <path>  local checkout of a git repo in Spira's repo-map;
#                          a trivial bead is filed here and must land
#   --prev-tag <tag>       previous release tag; enables upgrade (phase B),
#                          rollback (phase C), and aged-install upgrade (phase D)
#   --record               write PASS/FAIL as a git note on <tag>
#                          under refs/notes/acceptance
#   --file-defects         file a builder bead per FAIL, linked discovered-from sp-ewwwq
#   --bd-db <path>         bd database path (default: ~/spira-acceptance-test-db).
#                          In CI, pass the instance's own db so phase D migration
#                          checks exercise the real store.
#   --agent <path>         stub agent replacing claude; set SPIRA_AGENT to this path
#                          so the sentinel's aeons complete deterministically without
#                          a model credential
#
# PHASES
#   A. Fresh install: check prerequisites, download release tarball for <tag>,
#      install from tarball (activate.sh + install.sh --skip-build), assert all
#      native binaries in bin/ are executable, land a bead by ancestry, run
#      uninstall, verify clean state.
#   B. Upgrade: install from <prev-tag> tarball, capture unit set, deploy.sh <tag>,
#      verify no rollback occurred, verify .tag sidecar names <tag>.  [--prev-tag only]
#   C. Rollback: deploy.sh <prev-tag>, verify unit set matches pre-upgrade snapshot.
#      [--prev-tag only]
#   D. Aged-install upgrade: install <prev-tag> tarball into surviving state (db,
#      config from prior phases), seed beads and statutes, start world, deploy.sh
#      <tag>, assert bead/memory counts preserved through migration, doctor no fatal,
#      operator override survived, no crash-loop units, world resumed, one bead lands
#      after upgrade. Force rollback and assert it either succeeds with a healthy world
#      or is refused and names the blocking migration.  [--prev-tag only]
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
#   Point the script at a tag known to be broken (e.g. one predating sp-jcb1) and
#   confirm it exits non-zero. A FAIL on the broken tag proves the PASS on the good
#   tag is meaningful — not vacuous.
#
# EXIT
#   0  all phases PASS
#   1  one or more phases FAIL (evidence printed on stdout)
#   2  usage error or missing prerequisite
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# ---------------------------------------------------------------------------
pass=0; fail=0

_json_escape() {
    local _s="${1:-}"
    _s="${_s//\\/\\\\}"
    _s="${_s//\"/\\\"}"
    _s="${_s//$'\n'/\\n}"
    _s="${_s//$'\r'/\\r}"
    printf '%s' "$_s"
}

ok() {
    pass=$((pass+1))
    printf '  ok    %s\n' "$1"
    printf '{"phase":"%s","check":"%s","verdict":"ok","ts":"%s","elapsed":%d}\n' \
        "${_cur_phase:-?}" "$(_json_escape "$1")" \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "$(($(date +%s)-${_phase_start_ts:-0}))" >> "${_jsonl_file:-/dev/null}"
}

bad() {
    fail=$((fail+1))
    printf '  FAIL  %s: %s\n' "$1" "${2:-}"
    printf '{"phase":"%s","check":"%s","verdict":"fail","reason":"%s","ts":"%s","elapsed":%d}\n' \
        "${_cur_phase:-?}" "$(_json_escape "$1")" "$(_json_escape "${2:-}")" \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "$(($(date +%s)-${_phase_start_ts:-0}))" >> "${_jsonl_file:-/dev/null}"
    if [ "${_phase_snapped:-0}" = 0 ]; then
        _phase_snapped=1
        _take_snapshot "first-fail-${_cur_phase:-unknown}" || true
    fi
}

is0()     { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }
not0()    { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is_same() { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

_take_snapshot() {
    local _label="${1:-snap}"
    _snap_count=$((_snap_count+1))
    [ -n "${_forensics_dir:-}" ] || return 0
    local _sdir
    _sdir="$_forensics_dir/$(printf '%02d' "$_snap_count")-${_label}"
    mkdir -p "$_sdir" 2>/dev/null || return 0
    systemctl --user list-timers --all          > "$_sdir/timers.txt"      2>&1 || true
    systemctl --user list-units 'spira-*' --all > "$_sdir/units.txt"       2>&1 || true
    {
        systemctl --user list-units 'spira-*' --all --no-legend 2>/dev/null \
            | awk '{print $1}' \
            | while read -r _u; do
                printf '\n=== %s ===\n' "$_u"
                systemctl --user status "$_u" --no-pager -l 2>&1 || true
            done
    } > "$_sdir/unit-status.txt" 2>/dev/null || true
    journalctl --user --since="${_run_start_wall:-today}" --no-pager -l \
        > "$_sdir/journal-full.txt" 2>&1 || true
    mkdir -p "$_sdir/journal"
    systemctl --user list-units 'spira-*' --all --no-legend 2>/dev/null \
        | awk '{print $1}' \
        | while read -r _u; do
            journalctl --user -u "$_u" --since="${_run_start_wall:-today}" --no-pager -l \
                > "$_sdir/journal/${_u}.txt" 2>&1 || true
        done
    command -v bd >/dev/null 2>&1 && {
        bd -C "$bd_db" list --all --json            > "$_sdir/bd-list.json"    2>&1 || true
        bd -C "$bd_db" ready --json                 > "$_sdir/bd-ready.json"   2>&1 || true
        [ -n "${_bead_id:-}" ] && \
            bd -C "$bd_db" show "$_bead_id" --json  > "$_sdir/bd-probe.json"   2>&1 || true
    }
    local _spira_run="${SPIRA_RUN:-${HOME}/.local/share/spira/run}"
    if [ -d "$_spira_run" ]; then
        ls -laR "$_spira_run"                       > "$_sdir/run-listing.txt" 2>&1 || true
        find "$_spira_run" -name '*.log' 2>/dev/null \
            | while read -r _lf; do cp "$_lf" "$_sdir/" 2>/dev/null || true; done
        cp "$_spira_run/landstate" "$_sdir/"                                    2>/dev/null || true
        cp "$_spira_run/queue"     "$_sdir/"                                    2>/dev/null || true
    fi
    cp "${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf" \
        "$_sdir/spira.conf" 2>/dev/null || true
    cp "${XDG_CONFIG_HOME:-$HOME/.config}/spira/repo-map" \
        "$_sdir/repo-map"   2>/dev/null || true
    git -C "${scratch_repo:-.}" rev-parse --git-dir >/dev/null 2>&1 && {
        git -C "$scratch_repo" log --all --oneline  > "$_sdir/scratch-log.txt"  2>&1 || true
        git -C "$scratch_repo" show-ref             > "$_sdir/scratch-refs.txt" 2>&1 || true
    }
    ps -ef --forest > "$_sdir/ps.txt"   2>&1 || true
    free -m         > "$_sdir/free.txt" 2>&1 || true
    df -h           > "$_sdir/df.txt"   2>&1 || true
    printf 'snapshot: %s\n' "$_sdir"
}

# ---------------------------------------------------------------------------
# ARG PARSING
# ---------------------------------------------------------------------------
tag=""
scratch_repo=""
prev_tag=""
do_record=0
do_file_defects=0
bd_db="${HOME}/spira-acceptance-test-db"
_agent=""

while [ $# -gt 0 ]; do
    case "$1" in
        --scratch-repo)  scratch_repo="${2:-}"; shift 2 ;;
        --scratch-repo=*) scratch_repo="${1#--scratch-repo=}"; shift ;;
        --prev-tag)      prev_tag="${2:-}"; shift 2 ;;
        --prev-tag=*)    prev_tag="${1#--prev-tag=}"; shift ;;
        --bd-db)         bd_db="${2:-}"; shift 2 ;;
        --bd-db=*)       bd_db="${1#--bd-db=}"; shift ;;
        --agent)         _agent="${2:-}"; shift 2 ;;
        --agent=*)       _agent="${1#--agent=}"; shift ;;
        --record)        do_record=1; shift ;;
        --file-defects)  do_file_defects=1; shift ;;
        -*)              printf 'acceptance-run: unknown option: %s\n' "$1" >&2; exit 2 ;;
        *)
            if [ -z "$tag" ]; then tag="$1"; else
                printf 'acceptance-run: too many positional arguments\n' >&2; exit 2
            fi
            shift ;;
    esac
done

if [ -z "$tag" ]; then
    printf 'usage: acceptance-run.sh <tag> --scratch-repo <path> [--prev-tag <tag>]\n' >&2
    exit 2
fi
if [ -z "$scratch_repo" ]; then
    printf 'acceptance-run: --scratch-repo is required\n' >&2
    exit 2
fi

# Derive the repo root (this file is in spira/, one level below the repo root).
REPO_ROOT="$(cd "$HERE/.." && pwd -P)"

TMP="$(mktemp -d)"
trap '_acceptance_cleanup' EXIT INT TERM
_acceptance_cleanup() {
    rm -rf "$TMP"
}

# Derive the forge repository (owner/repo) for gh release download calls.
_ar_gh_repo="${SPIRA_FORGE_REPO:-}"
[ -z "$_ar_gh_repo" ] && _ar_gh_repo="${GH_REPO:-}"
[ -z "$_ar_gh_repo" ] && _ar_gh_repo="${GITHUB_REPOSITORY:-}"
if [ -z "$_ar_gh_repo" ]; then
    _ar_remote="$(git -C "$REPO_ROOT" remote get-url origin 2>/dev/null)" || _ar_remote=""
    case "$_ar_remote" in
        https://github.com/*) _ar_gh_repo="${_ar_remote#https://github.com/}"; _ar_gh_repo="${_ar_gh_repo%.git}" ;;
        git@github.com:*)     _ar_gh_repo="${_ar_remote#git@github.com:}"; _ar_gh_repo="${_ar_gh_repo%.git}" ;;
    esac
    unset _ar_remote
fi

# _check_release_bins <release-dir> — print comma-separated list of missing bin/ entries.
_check_release_bins() {
    local _rd="$1" _missing=""
    for _b in loom panel broker spira-supervise; do
        [ -x "$_rd/bin/$_b" ] || _missing="${_missing:+$_missing, }bin/$_b"
    done
    printf '%s' "$_missing"
}

# _download_tarball <tag> <destdir> — download the release tarball; print path on stdout.
_download_tarball() {
    local _dtag="$1" _ddir="$2"
    local _dl_args=()
    [ -n "$_ar_gh_repo" ] && _dl_args+=(--repo "$_ar_gh_repo")
    gh "${_dl_args[@]}" release download "$_dtag" \
        --pattern 'spira-*.tar.gz' \
        --dir "$_ddir" >/dev/null 2>&1 || return 1
    ls "$_ddir"/spira-*.tar.gz 2>/dev/null | head -1
}

# _install_from_tarball <tarball> <releases-dir> <conf> — activate + install.sh --skip-build.
_install_from_tarball() {
    local _tb="$1" _rel="$2" _cf="$3"
    SPIRA_CONF="$_cf" SPIRA_RELEASES="$_rel" SPIRA_ACTIVATE_FORCE=1 \
        bash "$HERE/activate.sh" "$_tb" || return 1
}

_forensics_dir="${SPIRA_ACCEPTANCE_FORENSICS:-$TMP/forensics}"
_jsonl_file="$_forensics_dir/checks.jsonl"
_snap_count=0
_cur_phase="prerequisites"
_phase_snapped=0
_run_start_ts="$(date +%s)"
_run_start_wall="$(date -u '+%Y-%m-%d %H:%M:%S')"
_phase_start_ts="$_run_start_ts"
mkdir -p "$_forensics_dir"
: > "$_jsonl_file"

printf 'acceptance-run.sh  tag=%s\n' "$tag"
printf 'forensics:         %s\n' "$_forensics_dir"

# ===========================================================================
echo
echo "prerequisites — real tools on PATH"
# ===========================================================================
# Positive control: verify the prereq check would catch a missing tool.
# We confirm 'command -v' returns non-zero for a name that cannot exist.
if command -v "spira-acceptance-nonexistent-$$" >/dev/null 2>&1; then
    bad "positive-control: command -v catches missing tool" \
        "command -v returned 0 for a nonexistent name"
else
    ok "positive-control: command -v catches missing tool"
fi

for _tool in bd dolt git gh python3; do
    if command -v "$_tool" >/dev/null 2>&1; then
        ok "prereq: $_tool on PATH"
    else
        bad "prereq: $_tool on PATH" "not found; install before running acceptance"
    fi
done

# Check systemd --user is available (not just on PATH but functional).
# Uses is-system-running: the same probe doctor uses so a failure here blocks
# before doctor reaches the enabled-units check and produces one clear message.
_ar_mgr_out="$(systemctl --user is-system-running 2>/dev/null || true)"
if [ -n "$_ar_mgr_out" ]; then
    ok "prereq: systemd --user available"
else
    bad "prereq: systemd --user available" \
        "systemctl --user is-system-running produced no output; check XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS"
fi
unset _ar_mgr_out

# Scratch repo must exist.
if [ -d "$scratch_repo/.git" ] || git -C "$scratch_repo" rev-parse --git-dir >/dev/null 2>&1; then
    ok "prereq: scratch-repo is a git repository ($scratch_repo)"
else
    bad "prereq: scratch-repo is a git repository" "$scratch_repo is not a git repo"
fi

[ "$fail" -gt 0 ] && {
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    printf 'acceptance-run: prerequisite failures — cannot continue\n' >&2
    exit 2
}

# ===========================================================================
_cur_phase="phase-A"; _phase_snapped=0; _phase_start_ts="$(date +%s)"
echo
echo "phase A — positive control: binary check catches missing binary"
# ===========================================================================
_pc_rel="$TMP/pc-release"
mkdir -p "$_pc_rel/bin"
for _pcb in loom panel broker; do
    printf '#!/bin/sh\n' > "$_pc_rel/bin/$_pcb" && chmod +x "$_pc_rel/bin/$_pcb"
done
# spira-supervise intentionally absent
_pc_missing="$(_check_release_bins "$_pc_rel")"
if printf '%s' "$_pc_missing" | grep -q "spira-supervise"; then
    ok "positive-control: binary check names missing binary (spira-supervise)"
else
    bad "positive-control: binary check names missing binary" \
        "got: [${_pc_missing:-empty}]"
fi
unset _pc_rel _pc_missing _pcb

# ===========================================================================
echo
echo "phase A — fresh install from $tag tarball"
# ===========================================================================

# Set up the releases directory and pre-seed conf.
_releases="$TMP/releases"
mkdir -p "$_releases"
_conf="${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf"
mkdir -p "$(dirname "$_conf")"
{
    [ -n "$_agent" ] && printf 'SPIRA_AGENT = %s\n' "$_agent"
    printf 'SPIRA_OPERATED = 0\n'
    printf 'SPIRA_RELEASES = %s\n' "$_releases"
} > "$_conf"

# Download the release tarball.
_tarball_dir="$TMP/tarball-dl"
mkdir -p "$_tarball_dir"
_tarball_dl_rc=0
_tarball_file="$(_download_tarball "$tag" "$_tarball_dir")" || _tarball_dl_rc=$?
is0 "phase A: gh release download $tag" "$_tarball_dl_rc"
[ -n "${_tarball_file:-}" ] \
    && ok "phase A: tarball found: $(basename "$_tarball_file")" \
    || bad "phase A: tarball found" "no spira-*.tar.gz in $_tarball_dir"

# Compute sha256 of the candidate tarball (recorded in the acceptance note).
_tarball_sha256=""
[ -f "${_tarball_file:-}" ] && \
    _tarball_sha256="$(sha256sum "$_tarball_file" 2>/dev/null | awk '{print $1}')" || \
    _tarball_sha256="$(shasum -a 256 "${_tarball_file:-/dev/null}" 2>/dev/null | awk '{print $1}')"

# Activate the tarball: unpack into $releases, atomic current symlink.
_activate_rc=0
[ -f "${_tarball_file:-}" ] && \
    _install_from_tarball "$_tarball_file" "$_releases" "$_conf" \
    2>&1 | tee "$TMP/activate.log" || _activate_rc=${PIPESTATUS[0]}
is0 "phase A: activate.sh exits 0" "$_activate_rc"

# Assert every native binary the release ships is present and executable.
if [ -d "$_releases/current" ]; then
    _a_missing_bins="$(_check_release_bins "$_releases/current")"
    if [ -z "$_a_missing_bins" ]; then
        ok "phase A: all native binaries present and executable"
    else
        bad "phase A: all native binaries present and executable" "missing: $_a_missing_bins"
    fi
fi

# Run install.sh from the activated release (--skip-build: binaries are in bin/).
_install_rc=0
_install_env=(SPIRA_OPERATED=0
    SPIRA_CONF="$_conf"
    SPIRA_RELEASES="$_releases"
    SPIRA_HOME_REPO="$(basename "$scratch_repo")"
)
[ -n "$_agent" ] && _install_env+=(SPIRA_AGENT="$_agent")
[ -d "$_releases/current" ] && \
    env "${_install_env[@]}" bash "$_releases/current/install.sh" --skip-build \
    2>&1 | tee "$TMP/install.log" || _install_rc=${PIPESTATUS[0]:-$?}
is0 "phase A: install.sh exits 0" "$_install_rc"

# Read the builder's label keys from the release conf so the probe bead carries the
# labels the sentinel's predicate reads (law-absence-needs-a-positive-control).
_a_plan_label="plan"
_a_scope_label="$(basename "$scratch_repo")"
if [ "$_install_rc" -eq 0 ] && [ -f "$_releases/current/spira/conf.sh" ]; then
    _a_plan_label="$(SPIRA_CONF="$_conf" SPIRA_CONF_LOADED="" \
        SPIRA_HOME_REPO="$(basename "$scratch_repo")" \
        bash -c '. "$1" 2>/dev/null; printf "%s" "${SPIRA_PLAN_LABEL:-plan}"' \
        _ "$_releases/current/spira/conf.sh" 2>/dev/null)" || _a_plan_label="plan"
    [ -z "$_a_plan_label" ] && _a_plan_label="plan"
    _a_scope_label="$(SPIRA_CONF="$_conf" SPIRA_CONF_LOADED="" \
        SPIRA_HOME_REPO="$(basename "$scratch_repo")" \
        bash -c '. "$1" 2>/dev/null; printf "%s" "${SPIRA_SCOPE_LABEL}"' \
        _ "$_releases/current/spira/conf.sh" 2>/dev/null)" || _a_scope_label="$(basename "$scratch_repo")"
    [ -z "$_a_scope_label" ] && _a_scope_label="$(basename "$scratch_repo")"
fi

# After install, verify ready.sh exits 0.
_ready_rc=0
_ready_out="$(env "${_install_env[@]}" bash "$_releases/current/spira/ready.sh" 2>&1)" || _ready_rc=$?
if [ "$_ready_rc" -eq 0 ]; then
    ok "phase A: ready.sh exits 0 after install"
else
    bad "phase A: ready.sh exits 0 after install" "$_releases/current/spira/ready.sh exit $_ready_rc"
fi
[ "$_ready_rc" -eq 0 ] || printf '%s\n' "$_ready_out"

# ===========================================================================
echo
echo "phase A — end-to-end bead: file, sentinel summons aeon, land by ancestry"
# ===========================================================================

# Capture the scratch repo's land ref before filing the bead.
_land_ref="$(git -C "$scratch_repo" rev-parse --abbrev-ref HEAD 2>/dev/null)" || _land_ref="main"
_base_sha_before="$(git -C "$scratch_repo" rev-parse "origin/${_land_ref}" 2>/dev/null)" || \
    _base_sha_before="$(git -C "$scratch_repo" rev-parse "HEAD" 2>/dev/null)"

# File a trivial bead: "acceptance test: land this bead to prove the install works".
# Use a very short task description so an aeon can complete it in one pass without
# complex reasoning. The bead body says to commit an empty file.
_bead_title="acceptance-run: trivial land proof for $tag"
_bead_out=""
_bead_out="$(bd -C "$bd_db" create \
    --title "$_bead_title" \
    --description "Acceptance test: commit an empty file named acceptance-probe.txt to prove end-to-end landing works. Content: the tag under test is $tag." \
    --label "acceptance,${_a_plan_label},${_a_scope_label},repo:$(basename "$scratch_repo")" \
    --type task \
    2>&1)" || true
_bead_id="$(printf '%s\n' "$_bead_out" \
    | sed -n 's/.*Created issue: \([a-z0-9]*-[a-z0-9]*\).*/\1/p' | head -1)"

if [ -n "$_bead_id" ]; then
    ok "phase A: bead filed ($_bead_id)"
else
    bad "phase A: bead filed" "bd create output: $_bead_out"
fi

if [ -n "$_bead_id" ]; then
    # Check: the bead is claimable by the builder persona — immediately, not after
    # 6 minutes of polling. sentinel --report only shows SPIRA_GOAL children; it
    # cannot see a bead that is not under the goal epic, so the old polling loop
    # always timed out even when the sentinel was healthy and the bead was ready.
    # Claimability (bd ready) is the discriminating question: a bead the sentinel
    # can see but no predicate matches is indistinguishable from a silent sentinel
    # (law-absence-needs-a-positive-control).
    _bead_ready=0
    _a_ready_json="$(bd -C "$bd_db" ready \
        --label "${_a_scope_label},${_a_plan_label}" --limit 0 --json 2>/dev/null)" || true
    if printf '%s' "$_a_ready_json" \
        | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
    ids = {i.get('id') for i in (d if isinstance(d, list) else [d])}
    sys.exit(0 if '$_bead_id' in ids else 1)
except Exception:
    sys.exit(1)
" 2>/dev/null; then
        ok "phase A: bead is claimable by builder predicate (${_a_scope_label},${_a_plan_label})"
        _bead_ready=1
    else
        bad "phase A: bead is claimable by builder predicate (${_a_scope_label},${_a_plan_label})" \
            "bead $_bead_id not found in 'bd ready --label ${_a_scope_label},${_a_plan_label}' — builder predicate does not match bead labels"
    fi

    # Stage 2: Summoned — start sentinel directly, poll for aeon branch.
    _a_t2=$(date +%s)
    systemctl --user start spira-sentinel.service 2>/dev/null || true
    _a_summoned=0
    while [ $(( $(date +%s) - _a_t2 )) -lt 60 ]; do
        git -C "$scratch_repo" show-ref --verify -q \
            "refs/heads/spira/$_bead_id" 2>/dev/null && { _a_summoned=1; break; }
        sleep 2
    done
    _a_s2_elapsed=$(( $(date +%s) - _a_t2 ))
    if [ "$_a_summoned" -eq 1 ]; then
        ok "phase A stage 2: aeon summoned for $_bead_id — branch created (${_a_s2_elapsed}s)"
    else
        bad "phase A stage 2: aeon summoned for $_bead_id" \
            "branch spira/$_bead_id not created after ${_a_s2_elapsed}s"
    fi

    # Stage 3: Committed — branch has a commit naming the bead.
    _a_t3=$(date +%s)
    _a_committed=0
    while [ $(( $(date +%s) - _a_t3 )) -lt 60 ]; do
        git -C "$scratch_repo" log --oneline "spira/$_bead_id" 2>/dev/null \
            | grep -qF "$_bead_id" && { _a_committed=1; break; }
        sleep 2
    done
    _a_s3_elapsed=$(( $(date +%s) - _a_t3 ))
    if [ "$_a_committed" -eq 1 ]; then
        ok "phase A stage 3: commit on spira/$_bead_id for $_bead_id (${_a_s3_elapsed}s)"
    else
        bad "phase A stage 3: commit on spira/$_bead_id for $_bead_id" \
            "no commit naming bead id on branch after ${_a_s3_elapsed}s"
    fi

    # Stage 4: Closed — bead status is closed.
    _a_t4=$(date +%s)
    _a_closed=0
    while [ $(( $(date +%s) - _a_t4 )) -lt 30 ]; do
        _a_s4_st="$(bd -C "$bd_db" show "$_bead_id" --json 2>/dev/null \
            | sed -n '/^[[{]/,$p' \
            | python3 -c 'import json,sys
try:
    d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
    print(d[0].get("status","") if d else "")
except Exception: print("")' 2>/dev/null)" || _a_s4_st=""
        [ "$_a_s4_st" = "closed" ] && { _a_closed=1; break; }
        sleep 2
    done
    _a_s4_elapsed=$(( $(date +%s) - _a_t4 ))
    if [ "$_a_closed" -eq 1 ]; then
        ok "phase A stage 4: bead $_bead_id closed (${_a_s4_elapsed}s)"
    else
        bad "phase A stage 4: bead $_bead_id closed" \
            "not closed after ${_a_s4_elapsed}s"
    fi

    # Stage 5: Landed — commit on origin/$_land_ref by ancestry.
    # Kick sentinel: CHECK 6 dispatches landing.sh on the closed branch.
    systemctl --user start spira-sentinel.service 2>/dev/null || true
    _a_t5=$(date +%s)
    _a_landed=0
    while [ $(( $(date +%s) - _a_t5 )) -lt 120 ]; do
        git -C "$scratch_repo" fetch origin >/dev/null 2>&1 || true
        _base_sha_now="$(git -C "$scratch_repo" rev-parse "origin/${_land_ref}" 2>/dev/null)" \
            || _base_sha_now="$_base_sha_before"
        if [ "$_base_sha_now" != "$_base_sha_before" ] \
            && git -C "$scratch_repo" log --format='%s' "$_base_sha_before..$_base_sha_now" 2>/dev/null \
               | grep -qF "$_bead_id"; then
            _a_landed=1; break
        fi
        sleep 5
    done
    _a_s5_elapsed=$(( $(date +%s) - _a_t5 ))
    if [ "$_a_landed" -eq 1 ]; then
        ok "phase A stage 5: bead $_bead_id landed on $scratch_repo:$_land_ref (${_a_s5_elapsed}s)"
    else
        bad "phase A stage 5: bead $_bead_id landed on $scratch_repo:$_land_ref" \
            "no commit with bead id on origin/$_land_ref after ${_a_s5_elapsed}s"
    fi
fi

# ===========================================================================
echo
echo "phase A — uninstall and clean state"
# ===========================================================================
_uninstall_out="$(bash "$HERE/uninstall.sh" --yes 2>&1)"
is0 "phase A: uninstall.sh --yes exits 0" "$?"

# Verify no spira-* units remain.
_units_after="$(systemctl --user list-unit-files --no-legend 2>/dev/null \
    | awk '{print $1}' | grep '^spira-' || true)"
[ -z "$_units_after" ] \
    && ok "phase A: no spira-* units remain after uninstall" \
    || bad "phase A: no spira-* units remain after uninstall" "$_units_after"

# ===========================================================================
_cur_phase="phase-B"; _phase_snapped=0; _phase_start_ts="$(date +%s)"
echo
echo "phase B — upgrade: install $prev_tag, deploy to $tag, verify no rollback"
# ===========================================================================
if [ -z "$prev_tag" ]; then
    printf '  skip  phase B+C: --prev-tag not given\n'
else
    # Download and install from prev_tag tarball.
    _prev_tb_dir="$TMP/prev-tarball-dl"
    mkdir -p "$_prev_tb_dir"
    _prev_tarball_dl_rc=0
    _prev_tarball_file="$(_download_tarball "$prev_tag" "$_prev_tb_dir")" || _prev_tarball_dl_rc=$?
    if [ "$_prev_tarball_dl_rc" -ne 0 ] || [ -z "${_prev_tarball_file:-}" ]; then
        bad "phase B: gh release download $prev_tag" "rc=$_prev_tarball_dl_rc"
    else
        ok "phase B: prev tarball downloaded: $(basename "$_prev_tarball_file")"

        _install_from_tarball "$_prev_tarball_file" "$_releases" "$_conf" \
            2>&1 | tee "$TMP/prev-activate.log" || true

        _prev_install_rc=0
        env "${_install_env[@]}" bash "$_releases/current/install.sh" --skip-build \
            2>&1 | tee "$TMP/prev-install.log" || _prev_install_rc=${PIPESTATUS[0]:-$?}
        is0 "phase B: install.sh ($prev_tag) exits 0" "$_prev_install_rc"

        # Guard: a failed install leaves the database service down; deploy.sh would
        # then report connection refused — which looks like an upgrade failure rather
        # than an install failure. Gate phases B and C on install success so the
        # failure is attributed to the right phase.
        if [ "$_prev_install_rc" -ne 0 ]; then
            bad "phase B+C: skipped — install failed; database service not started" \
                "prev_install_rc=$_prev_install_rc"
        else

        # Capture unit set BEFORE upgrade.
        _units_pre_upgrade="$(systemctl --user list-unit-files --no-legend 2>/dev/null \
            | awk '{print $1, $2}' | grep '^spira-' | sort || true)"

        # Run deploy.sh to upgrade to newest tag.
        _deploy_rc=0
        bash "$HERE/deploy.sh" "$tag" 2>&1 | tee "$TMP/deploy-upgrade.log" || _deploy_rc=$?
        is0 "phase B: deploy.sh $tag exits 0 (no rollback)" "$_deploy_rc"

        # Verify .tag sidecar names the new tag (sp-cb0q1: sidecar written to releases dir).
        # Read SPIRA_RELEASES from the installed conf so it matches what deploy.sh used,
        # not a default derived from the workspace checkout (which differs on CI runners).
        _releases_dir="$(bash -c '. "$1/conf.sh" 2>/dev/null; printf "%s" "${SPIRA_RELEASES:-}"' \
            -- "$HERE" 2>/dev/null || true)"
        [ -z "$_releases_dir" ] && _releases_dir="${SPIRA_RELEASES:-${HOME}/spira-releases}"
        # deploy.sh writes the full release tag to .tags/<release_stem> beside the release
        # dirs; current/.tag does not exist (release dirs are read-only after activate.sh).
        _current_rel="$(readlink "${_releases_dir}/current" 2>/dev/null || true)"
        _tag_sidecar="${_releases_dir}/.tags/${_current_rel:-}"
        _sidecar_val="$(cat "$_tag_sidecar" 2>/dev/null || printf '')"
        is_same "phase B: .tag sidecar names $tag" "$tag" "$_sidecar_val"

        # Verify SPIRA_PROD points into the releases directory (not the raw checkout).
        _spira_prod="$(bash -c '. "$1/conf.sh" 2>/dev/null; printf "%s" "${SPIRA_PROD:-}"' \
            -- "$HERE" 2>/dev/null || true)"
        want "phase B: SPIRA_PROD updated to releases path" "${_releases_dir}" "$_spira_prod"

        # ===========================================================================
        _cur_phase="phase-C"; _phase_snapped=0; _phase_start_ts="$(date +%s)"
        echo
        echo "phase C — rollback: deploy $prev_tag, verify unit set restored"
        # ===========================================================================

        _rollback_rc=0
        bash "$HERE/deploy.sh" "$prev_tag" 2>&1 | tee "$TMP/deploy-rollback.log" \
            || _rollback_rc=$?
        is0 "phase C: deploy.sh $prev_tag (rollback) exits 0" "$_rollback_rc"

        # Capture unit set AFTER rollback.
        _units_post_rollback="$(systemctl --user list-unit-files --no-legend 2>/dev/null \
            | awk '{print $1, $2}' | grep '^spira-' | sort || true)"

        # Verify the unit set is identical to the pre-upgrade snapshot.
        # sp-x6ygl: rollback must restore the prior release's ExecStart paths AND unit set.
        _unit_diff="$(diff \
            <(printf '%s\n' "$_units_pre_upgrade") \
            <(printf '%s\n' "$_units_post_rollback") || true)"
        [ -z "$_unit_diff" ] \
            && ok "phase C: unit set after rollback matches pre-upgrade snapshot (no extra/dropped units)" \
            || bad "phase C: unit set after rollback matches pre-upgrade snapshot" \
                   "$(printf '%s\n' "$_unit_diff" | head -10)"

        # Final uninstall.
        bash "$HERE/uninstall.sh" --yes >/dev/null 2>&1
        is0 "phase C: uninstall.sh --yes after rollback exits 0" "$?"

        fi  # end guard: phase B install succeeded
    fi
fi

# ===========================================================================
_cur_phase="phase-D"; _phase_snapped=0; _phase_start_ts="$(date +%s)"
echo
echo "phase D — aged-install upgrade: $prev_tag with real state → $tag"
# ===========================================================================
# Install prev_tag into surviving state (db and config persist from phases A-C;
# uninstall.sh --yes removes units but not the database or config file). Seed
# additional beads and memories to guarantee the store is populated, write an
# operator override, start the world, then upgrade via deploy.sh. Assert that
# migration preserved bead/memory counts, doctor reports no fatal, the operator
# override survived, no units crash-loop, and the world can land new work.
# Finally force a rollback and assert it either succeeds cleanly or is refused
# with the blocking migration named (law-pin-by-migration-count).
if [ -z "$prev_tag" ]; then
    printf '  skip  phase D: --prev-tag not given\n'
else
    # Reuse the prev_tag tarball (already downloaded for phase B if --prev-tag was given).
    _aged_tb_dir="$TMP/prev-tarball-dl"
    [ -d "$_aged_tb_dir" ] || mkdir -p "$_aged_tb_dir"
    _aged_tarball_file="$(ls "$_aged_tb_dir"/spira-*.tar.gz 2>/dev/null | head -1)"
    if [ -z "${_aged_tarball_file:-}" ]; then
        _aged_tb_rc=0
        _aged_tarball_file="$(_download_tarball "$prev_tag" "$_aged_tb_dir")" || _aged_tb_rc=$?
        is0 "phase D: gh release download $prev_tag (aged base)" "$_aged_tb_rc"
    else
        ok "phase D: prev tarball already present: $(basename "$_aged_tarball_file")"
    fi

    # Conf already has SPIRA_RELEASES from phase A; surviving state is intentional.
    _aged_conf="$_conf"
    _aged_install_rc=0
    _aged_env=(SPIRA_OPERATED=0
        SPIRA_CONF="$_aged_conf"
        SPIRA_RELEASES="$_releases"
        SPIRA_HOME_REPO="$(basename "$scratch_repo")"
    )
    [ -n "$_agent" ] && _aged_env+=(SPIRA_AGENT="$_agent")

    [ -f "${_aged_tarball_file:-}" ] && \
        _install_from_tarball "$_aged_tarball_file" "$_releases" "$_aged_conf" \
        2>&1 | tee "$TMP/aged-activate.log" || true

    env "${_aged_env[@]}" bash "$_releases/current/install.sh" --skip-build \
        2>&1 | tee "$TMP/aged-install.log" || _aged_install_rc=${PIPESTATUS[0]:-$?}
    is0 "phase D: install.sh ($prev_tag, aged) exits 0" "$_aged_install_rc"

    # Verify ready.sh exits 0 after aged install.
    _aged_ready_rc=0
    _aged_ready_out="$(env "${_aged_env[@]}" bash "$_releases/current/spira/ready.sh" 2>&1)" \
        || _aged_ready_rc=$?
    if [ "$_aged_ready_rc" -eq 0 ]; then
        ok "phase D: ready.sh exits 0 after aged install"
    else
        bad "phase D: ready.sh exits 0 after aged install" "$_releases/current/spira/ready.sh exit $_aged_ready_rc"
        printf '%s\n' "$_aged_ready_out"
    fi

    if [ "$_aged_install_rc" -eq 0 ]; then
        # Seed beads into $bd_db (the instance's db in CI).
        bd -C "$bd_db" create \
            --title "aged-install: open seed bead (pre-upgrade)" \
            --label "acceptance-seed" --type task >/dev/null 2>&1 || true
        _aged_seed2_out=""
        _aged_seed2_out="$(bd -C "$bd_db" create \
            --title "aged-install: closed seed bead (pre-upgrade)" \
            --label "acceptance-seed" --type task 2>&1)" || true
        _aged_seed2="$(printf '%s\n' "$_aged_seed2_out" \
            | sed -n 's/.*Created issue: \([a-z0-9]*-[a-z0-9]*\).*/\1/p' | head -1)"
        [ -n "$_aged_seed2" ] && \
            bd -C "$bd_db" close "$_aged_seed2" \
                --reason "acceptance: closed for aged-install migration test" \
                >/dev/null 2>&1 || true

        # Seed statute-style memories.
        bd -C "$bd_db" remember \
            "aged-install acceptance seed: statute 1 for migration verification" \
            --key "law-acceptance-aged-seed-1" >/dev/null 2>&1 || true
        bd -C "$bd_db" remember \
            "aged-install acceptance seed: statute 2 for migration verification" \
            --key "law-acceptance-aged-seed-2" >/dev/null 2>&1 || true

        # Capture pre-upgrade counts.
        _aged_pre_beads="$(bd -C "$bd_db" list --all --json 2>/dev/null \
            | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null \
            || printf 0)"
        _aged_pre_mems="$(bd -C "$bd_db" memories 2>/dev/null | grep -c . 2>/dev/null \
            || printf 0)"

        # Write an operator override into spira.conf to verify it survives the upgrade.
        _aged_override_val="aged-install-override-$$"
        printf '\nACCEPTANCE_AGED_OVERRIDE = %s\n' "$_aged_override_val" >> "$_aged_conf"

        # Start world and confirm the sentinel timer is active.
        bash "$HERE/world.sh" start 2>&1 | tee "$TMP/aged-world-start.log" || true
        _aged_sentinel="$(systemctl --user list-units --state=active --no-legend 2>/dev/null \
            | awk '{print $1}' | grep 'spira-sentinel' | head -1)"
        [ -n "$_aged_sentinel" ] \
            && ok "phase D: sentinel timer active (world live before upgrade)" \
            || bad "phase D: sentinel timer active (world live before upgrade)" \
                   "no spira-sentinel* in active units"

        # Brief window: prove the world is live before upgrading.
        sleep 30

        # Upgrade to tag from the aged, populated state.
        _aged_deploy_rc=0
        bash "$HERE/deploy.sh" "$tag" 2>&1 | tee "$TMP/aged-deploy.log" \
            || _aged_deploy_rc=$?
        is0 "phase D: deploy.sh $tag (aged upgrade) exits 0 — no rollback" "$_aged_deploy_rc"

        if [ "$_aged_deploy_rc" -eq 0 ]; then

            # Assert: bead count preserved through migration (no rows dropped).
            _aged_post_beads="$(bd -C "$bd_db" list --all --json 2>/dev/null \
                | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null \
                || printf 0)"
            [ "$_aged_post_beads" -ge "$_aged_pre_beads" ] \
                && ok "phase D: bead count preserved through migration ($_aged_pre_beads → $_aged_post_beads)" \
                || bad "phase D: bead count preserved through migration" \
                       "before=$_aged_pre_beads after=$_aged_post_beads — rows lost"

            # Assert: memory count preserved through migration.
            _aged_post_mems="$(bd -C "$bd_db" memories 2>/dev/null | grep -c . 2>/dev/null \
                || printf 0)"
            [ "$_aged_post_mems" -ge "$_aged_pre_mems" ] \
                && ok "phase D: memory count preserved through migration ($_aged_pre_mems → $_aged_post_mems)" \
                || bad "phase D: memory count preserved through migration" \
                       "before=$_aged_pre_mems after=$_aged_post_mems — rows lost"

            # Assert: doctor.sh exits 0 (no fatal).
            _aged_doctor_rc=0
            bash "$HERE/doctor.sh" 2>&1 | tee "$TMP/aged-doctor.log" \
                || _aged_doctor_rc=$?
            is0 "phase D: doctor.sh no fatal after aged upgrade" "$_aged_doctor_rc"

            # Assert: operator override survived in spira.conf.
            _aged_override_got="$(grep -E '^\s*ACCEPTANCE_AGED_OVERRIDE\s*=' \
                "$_aged_conf" 2>/dev/null \
                | sed 's/[^=]*=\s*//' | head -1 || true)"
            is_same "phase D: operator override survived aged upgrade" \
                "$_aged_override_val" "$_aged_override_got"

            # Assert: no failed spira units 2 min after upgrade (crash-loop check).
            sleep 120
            _aged_failed="$(systemctl --user list-units --state=failed --no-legend \
                2>/dev/null | awk '{print $1}' | grep '^spira-' || true)"
            [ -z "$_aged_failed" ] \
                && ok "phase D: no failed spira units 2 min after aged upgrade" \
                || bad "phase D: no failed spira units 2 min after aged upgrade" \
                       "$_aged_failed"

            # Assert: world resumed after upgrade (no HALTED/STOPPED state).
            _aged_world_out="$(bash "$HERE/world.sh" status 2>&1)"
            if printf '%s' "$_aged_world_out" | grep -qE 'HALTED|STOPPED'; then
                bad "phase D: world running after aged upgrade" \
                    "$(printf '%s' "$_aged_world_out" \
                       | grep -E 'HALTED|STOPPED' | head -1)"
            else
                ok "phase D: world running after aged upgrade"
            fi

            # Assert: file a bead post-upgrade; verify it lands by ancestry.
            _aged_land_base="$(git -C "$scratch_repo" \
                rev-parse "origin/${_land_ref:-main}" 2>/dev/null)" || _aged_land_base=""
            _aged_probe_out=""
            _aged_probe_out="$(bd -C "$bd_db" create \
                --title "aged-install: post-upgrade land proof ($prev_tag → $tag)" \
                --description "Prove world resumed and can land work after aged upgrade from $prev_tag to $tag." \
                --label "acceptance,${_a_plan_label},${_a_scope_label},repo:$(basename "$scratch_repo")" \
                --type task 2>&1)" || true
            _aged_probe_id="$(printf '%s\n' "$_aged_probe_out" \
                | sed -n 's/.*Created issue: \([a-z0-9]*-[a-z0-9]*\).*/\1/p' | head -1)"
            if [ -n "$_aged_probe_id" ]; then
                ok "phase D: post-upgrade bead filed ($_aged_probe_id)"

                # Stage 2: Summoned — start sentinel, poll for aeon branch.
                _d_t2=$(date +%s)
                systemctl --user start spira-sentinel.service 2>/dev/null || true
                _d_summoned=0
                while [ $(( $(date +%s) - _d_t2 )) -lt 60 ]; do
                    git -C "$scratch_repo" show-ref --verify -q \
                        "refs/heads/spira/$_aged_probe_id" 2>/dev/null \
                        && { _d_summoned=1; break; }
                    sleep 2
                done
                _d_s2_elapsed=$(( $(date +%s) - _d_t2 ))
                if [ "$_d_summoned" -eq 1 ]; then
                    ok "phase D stage 2: aeon summoned for $_aged_probe_id — branch created (${_d_s2_elapsed}s)"
                else
                    bad "phase D stage 2: aeon summoned for $_aged_probe_id" \
                        "branch spira/$_aged_probe_id not created after ${_d_s2_elapsed}s"
                fi

                # Stage 3: Committed — branch has a commit naming the bead.
                _d_t3=$(date +%s)
                _d_committed=0
                while [ $(( $(date +%s) - _d_t3 )) -lt 60 ]; do
                    git -C "$scratch_repo" log --oneline "spira/$_aged_probe_id" 2>/dev/null \
                        | grep -qF "$_aged_probe_id" && { _d_committed=1; break; }
                    sleep 2
                done
                _d_s3_elapsed=$(( $(date +%s) - _d_t3 ))
                if [ "$_d_committed" -eq 1 ]; then
                    ok "phase D stage 3: commit on spira/$_aged_probe_id (${_d_s3_elapsed}s)"
                else
                    bad "phase D stage 3: commit on spira/$_aged_probe_id" \
                        "no commit naming bead id on branch after ${_d_s3_elapsed}s"
                fi

                # Stage 4: Closed — bead status is closed.
                _d_t4=$(date +%s)
                _d_closed=0
                while [ $(( $(date +%s) - _d_t4 )) -lt 30 ]; do
                    _d_s4_st="$(bd -C "$bd_db" show "$_aged_probe_id" --json 2>/dev/null \
                        | sed -n '/^[[{]/,$p' \
                        | python3 -c 'import json,sys
try:
    d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
    print(d[0].get("status","") if d else "")
except Exception: print("")' 2>/dev/null)" || _d_s4_st=""
                    [ "$_d_s4_st" = "closed" ] && { _d_closed=1; break; }
                    sleep 2
                done
                _d_s4_elapsed=$(( $(date +%s) - _d_t4 ))
                if [ "$_d_closed" -eq 1 ]; then
                    ok "phase D stage 4: bead $_aged_probe_id closed (${_d_s4_elapsed}s)"
                else
                    bad "phase D stage 4: bead $_aged_probe_id closed" \
                        "not closed after ${_d_s4_elapsed}s"
                fi

                # Stage 5: Landed — commit on origin/land_ref by ancestry.
                systemctl --user start spira-sentinel.service 2>/dev/null || true
                _d_t5=$(date +%s)
                _aged_landed=0
                while [ $(( $(date +%s) - _d_t5 )) -lt 120 ]; do
                    git -C "$scratch_repo" fetch origin >/dev/null 2>&1 || true
                    _aged_sha_now="$(git -C "$scratch_repo" \
                        rev-parse "origin/${_land_ref:-main}" 2>/dev/null)" \
                        || _aged_sha_now="${_aged_land_base:-}"
                    if [ -n "${_aged_land_base:-}" ] \
                        && [ "$_aged_sha_now" != "$_aged_land_base" ] \
                        && git -C "$scratch_repo" log --format='%s' "$_aged_land_base..$_aged_sha_now" 2>/dev/null \
                           | grep -qF "$_aged_probe_id"; then
                        _aged_landed=1; break
                    fi
                    sleep 5
                done
                _d_s5_elapsed=$(( $(date +%s) - _d_t5 ))
                [ "$_aged_landed" -eq 1 ] \
                    && ok "phase D stage 5: bead $_aged_probe_id landed by ancestry (${_d_s5_elapsed}s)" \
                    || bad "phase D stage 5: bead $_aged_probe_id landed by ancestry" \
                           "no commit with $_aged_probe_id on origin/${_land_ref:-main} after ${_d_s5_elapsed}s"
            else
                bad "phase D: post-upgrade bead filed" "output: $_aged_probe_out"
            fi
        fi

        # Rollback: deploy prev_tag. A schema migration that prevents downgrade must
        # cause deploy to REFUSE and name the migration (law-pin-by-migration-count).
        echo
        echo "phase D — aged rollback: deploy $prev_tag (refuse-or-succeed)"
        _aged_rollback_rc=0
        _aged_rollback_out="$(bash "$HERE/deploy.sh" "$prev_tag" 2>&1)" \
            || _aged_rollback_rc=$?
        if [ "$_aged_rollback_rc" -ne 0 ]; then
            if printf '%s' "$_aged_rollback_out" | grep -qi 'migrat'; then
                ok "phase D: rollback refused — names migration (law-pin-by-migration-count)"
            else
                bad "phase D: rollback refused but output does not name migration" \
                    "$(printf '%s' "$_aged_rollback_out" | tail -5)"
            fi
        else
            ok "phase D: rollback to $prev_tag succeeded"
            _aged_rollback_world="$(bash "$HERE/world.sh" status 2>&1)"
            if printf '%s' "$_aged_rollback_world" | grep -qE 'HALTED|STOPPED'; then
                bad "phase D: world running after aged rollback" \
                    "$(printf '%s' "$_aged_rollback_world" \
                       | grep -E 'HALTED|STOPPED' | head -1)"
            else
                ok "phase D: world running after aged rollback"
            fi
        fi

        bash "$HERE/uninstall.sh" --yes >/dev/null 2>&1
        is0 "phase D: uninstall.sh exits 0" "$?"
    fi
fi

# ===========================================================================
_take_snapshot "end-of-run" || true
echo
printf '%d passed, %d failed\n' "$pass" "$fail"

verdict="FAIL"
[ "$fail" -eq 0 ] && verdict="PASS"

printf 'verdict: %s  tag=%s  date=%s\n' "$verdict" "$tag" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

# ---------------------------------------------------------------------------
# Record verdict as a git note on the tag so it is retrievable later.
# ---------------------------------------------------------------------------
if [ "$do_record" -eq 1 ]; then
    _note="$(printf '%s\n%s %s  %d passed, %d failed\n' \
        "$verdict" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$tag" "$pass" "$fail")"
    [ -n "${_tarball_sha256:-}" ] && \
        _note="$(printf '%s\nsha256:%s tarball:%s\n' \
            "$_note" "$_tarball_sha256" "$(basename "${_tarball_file:-unknown}")")"
    # Include (from, to) pair so phase D results are queryable per upgrade path.
    if [ -n "$prev_tag" ]; then
        _note="$(printf '%s\naged-install from=%s: %s\n' "$_note" "$prev_tag" "$verdict")"
    fi
    git -C "$REPO_ROOT" notes --ref=acceptance add -f -m "$_note" "refs/tags/$tag" \
        && printf 'recorded: git notes --ref=acceptance show refs/tags/%s\n' "$tag" \
        || printf 'warning: could not write git note (verdict still printed above)\n'
fi

# ---------------------------------------------------------------------------
# File defect beads for each failure if requested.
# ---------------------------------------------------------------------------
if [ "$do_file_defects" -eq 1 ] && [ "$fail" -gt 0 ]; then
    bd -C "$bd_db" create \
        --title "acceptance-run $tag: $fail phase(s) failed" \
        --label "defect,repo:spira,discovered-from:sp-ewwwq" \
        --type bug \
        --description "acceptance-run.sh found $fail failure(s) against tag $tag. See the acceptance-run log for detail." \
        >/dev/null 2>&1 || true
fi

[ -n "${_forensics_dir:-}" ] && printf 'forensics: %s\n' "$_forensics_dir"
[ "$fail" -eq 0 ]
