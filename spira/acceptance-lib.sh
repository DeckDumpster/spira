#!/usr/bin/env bash
# acceptance-lib.sh — acceptance-run.sh's phase structure, sourced rather than copied.
#
# Sourced, never executed:
#   . "$HERE/acceptance-lib.sh"
#
# Every function here reads or writes the caller's globals (pass, fail, _cur_phase,
# _phase_start_ts, _phase_snapped, _jsonl_file, _forensics_dir, _snap_count, bd_db,
# _bead_id, scratch_repo) rather than its own — this is acceptance-run.sh's phase
# structure lifted out, not a new abstraction with its own state.
#
# covers: spira/acceptance-run.sh
set -u

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

# _take_snapshot <label> — dump systemd/bd/run-dir/scratch-repo state under
# _forensics_dir/NN-<label>. Never lets a forensics failure change the verdict:
# every command here is best-effort, and a missing _forensics_dir is a silent no-op.
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

# _check_release_bins <release-dir> — print comma-separated list of missing bin/ entries.
_check_release_bins() {
    local _rd="$1" _missing=""
    for _b in loom panel broker spira-supervise landing-pass; do
        [ -x "$_rd/bin/$_b" ] || _missing="${_missing:+$_missing, }bin/$_b"
    done
    printf '%s' "$_missing"
}

# _download_tarball <tag> <destdir> — download the release tarball; print path on stdout.
_download_tarball() {
    local _dtag="$1" _ddir="$2"
    local _dl_args=()
    [ -n "${_ar_gh_repo:-}" ] && _dl_args+=(--repo "$_ar_gh_repo")
    gh "${_dl_args[@]}" release download "$_dtag" \
        --pattern 'spira-*.tar.gz' \
        --dir "$_ddir" >/dev/null 2>&1 || return 1
    ls "$_ddir"/spira-*.tar.gz 2>/dev/null | head -1
}

# _acquire_tarball <tag> <given-path> <download-dir> — print the tarball path on
# stdout. <given-path> non-empty (acceptance-run.sh's --tarball): use it
# directly and never call gh at all — acceptance-local.sh's own build has
# nothing published yet to download. Empty: download <tag> via
# _download_tarball, unchanged. Nothing prints on failure.
_acquire_tarball() {
    local _at_tag="$1" _at_given="$2" _at_dir="$3"
    if [ -n "$_at_given" ]; then
        [ -f "$_at_given" ] || return 1
        printf '%s' "$_at_given"
        return 0
    fi
    _download_tarball "$_at_tag" "$_at_dir"
}

# _install_from_tarball <tarball> <releases-dir> <conf> — activate + install.sh --skip-build.
_install_from_tarball() {
    local _tb="$1" _rel="$2" _cf="$3"
    SPIRA_CONF="$_cf" SPIRA_RELEASES="$_rel" SPIRA_ACTIVATE_FORCE=1 \
        bash "$HERE/activate.sh" "$_tb" || return 1
}

# _extract_bead_id <bd-create-output> — the bead id from `bd create`'s "Created issue:"
# line, ignoring any warning lines printed before it (GH#2950: beads.role not configured
# prints a warning ahead of the created-issue line on some bd versions).
_extract_bead_id() {
    printf '%s\n' "$1" \
        | sed -n 's/.*Created issue: \([a-z0-9]*-[a-z0-9]*\).*/\1/p' \
        | head -1
}

# _phase_env <array-name> <conf> <releases> <home-repo> [agent] — populate <array-name>
# with the install environment a phase runs under. One formula shared by every phase
# instead of one copy per phase, so SPIRA_OPERATED=0 cannot go missing from just one.
_phase_env() {
    local -n _pe_arr="$1"
    _pe_arr=(SPIRA_OPERATED=0
        SPIRA_CONF="$2"
        SPIRA_RELEASES="$3"
        SPIRA_HOME_REPO="$4"
    )
    [ -n "${5:-}" ] && _pe_arr+=(SPIRA_AGENT="$5")
}

# _run_ready <ready.sh path> [env=VAL ...] — run ready.sh under the given environment,
# print its combined output, and return its exit code. The exit code MUST be read from
# this function's own return, not from $? after the command substitution that captures
# its output (law-status-after-a-pipe-is-the-last-command): the one-line idiom this
# replaces was copied into two phases, and copying it a third time is how it would have
# drifted into `|| true` again.
_run_ready() {
    local _rs="$1"; shift
    local _out _rc=0
    _out="$(env "$@" bash "$_rs" 2>&1)" || _rc=$?
    printf '%s' "$_out"
    return "$_rc"
}
