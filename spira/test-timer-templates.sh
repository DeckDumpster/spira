#!/usr/bin/env bash
#
# test-timer-templates.sh — the T0 unit-file lint: every property of a shipped systemd/
# template that a static read of the file (or of units.sh) can prove, in one pass over the
# unit files instead of six suites each re-reading them.
#
# tier: T0
# covers: systemd/*.timer systemd/*.service systemd/units.sh systemd/render.py systemd/concierge.service systemd/beads-push.service spira/spira-verdict.sh spira/collect.sh supervise/** UC-instance-lifecycle-31
#
# WHAT THIS GUARDS. Defect sp-7gklu: a timer template existed in systemd/ but was absent from
# units.sh's UNITS array, so install.sh never wrote it to disk. Defect sp-mplcb: WatchdogSec
# was wired onto a bash ExecStart target, which cannot deliver the heartbeat systemd needs.
# Defect sp-vhvyi: spira-verdict.service's TimeoutStartSec was too short for a full replay.
# Defect sp-tv7ue: nothing proved the verdict timer's service actually called queue.sh step.
# Split-checkout deploys: an ExecStart pointing at @SPIRA_REPO@ resolves into the git checkout
# instead of the release tree.
#
# POSITIVE CONTROLS ARE FIRST (law-absence-needs-a-positive-control) in every section: a
# parser or extractor that returns empty or wrong content would make every absence verdict
# below look like a pass for the wrong reason.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"
UNITS_SH="$UNIT_DIR/units.sh"

echo "test-timer-templates.sh"

# ============================================================================
echo
echo "Parse units.sh — UNITS, ENABLE and OPTIONAL:"
# ============================================================================

[ -r "$UNITS_SH" ] || bail "units.sh not readable at $UNITS_SH"

units_block="$(awk '/^UNITS=\(/{found=1} found{print} found && /\)/{found=0}' "$UNITS_SH")"
enable_block="$(awk '/_ENABLE_TMPL=\(/{found=1} found{print} found && /\)/{found=0}' "$UNITS_SH")"
[ -n "$units_block" ] || bail "UNITS block parseable — awk found nothing; remaining checks are invalid"
[ -n "$enable_block" ] || bail "_ENABLE_TMPL block parseable — awk found nothing; remaining checks are invalid"
ok "UNITS block parseable (${#units_block} bytes)"
ok "_ENABLE_TMPL block parseable (${#enable_block} bytes)"

# CONDITIONAL UNITS ARE STILL INSTALLED UNITS. units.sh appends some template names inside an
# `if` (UNITS+=/ENABLE+=) and records the declined case in OPTIONAL+=. The static array
# literal above cannot see those lines, so the membership tests below also read every
# UNITS+=/ENABLE+= append line, and a timer that is only conditional must appear in an
# OPTIONAL+= line too — that keeps "never silently absent" true while letting a box decline
# a unit on purpose and say so.
units_all="$units_block
$(grep -E '^[[:space:]]*UNITS\+=\(' "$UNITS_SH")"
enable_all="$enable_block
$(grep -E '^[[:space:]]*ENABLE\+=\(' "$UNITS_SH")"
optional_block="$(grep -E '^[[:space:]]*OPTIONAL\+=\(' "$UNITS_SH")"
is_optional() { case "$optional_block" in *"$1"*) return 0 ;; *) return 1 ;; esac; }

# POSITIVE CONTROL: spira-sentinel.timer is the canary. If the parsers are broken or the
# blocks misidentified, it fails loudly here rather than making every absence verdict below
# a silent all-clear.
want "positive control: spira-sentinel.timer is in UNITS" "spira-sentinel.timer" "$units_block"
want "positive control: spira-sentinel.timer is in _ENABLE_TMPL" "spira-sentinel.timer" "$enable_block"

# ============================================================================
echo
echo "Every .timer template is in UNITS (or its decline recorded in OPTIONAL):"
# ============================================================================

timer_count=0
for tmr in "$UNIT_DIR"/*.timer; do
    [ -e "$tmr" ] || continue
    name="$(basename "$tmr")"
    timer_count=$((timer_count + 1))
    case "$units_all" in
        *"$name"*)
            case "$units_block" in
                *"$name"*) ok "UNITS: $name" ;;
                *)
                    if is_optional "$name"; then
                        ok "UNITS: $name (conditional, declined case in OPTIONAL)"
                    else
                        bad "UNITS: $name" "added conditionally but never recorded in OPTIONAL — a box that declines it cannot tell 'not installed here' from 'nobody listed it'"
                    fi ;;
            esac ;;
        *) bad "UNITS: $name" "absent from UNITS — install.sh will not write it to disk on a fresh install" ;;
    esac
done
[ "$timer_count" -gt 0 ] \
    && ok "$timer_count .timer templates found and checked against UNITS" \
    || bad "at least one .timer file in $UNIT_DIR" "glob matched nothing — check the path"

# ============================================================================
echo
echo "Every .timer template is in _ENABLE_TMPL (will be enabled):"
# ============================================================================

for tmr in "$UNIT_DIR"/*.timer; do
    [ -e "$tmr" ] || continue
    name="$(basename "$tmr")"
    case "$enable_all" in
        *"$name"*) ok "_ENABLE_TMPL: $name" ;;
        *) bad "_ENABLE_TMPL: $name" "absent — install.sh will install but not enable it; the timer will not fire" ;;
    esac
done

# ============================================================================
echo
echo "Every .timer template fires periodically (OnUnitActiveSec or OnCalendar):"
# ============================================================================

for tmr in "$UNIT_DIR"/*.timer; do
    [ -e "$tmr" ] || continue
    name="$(basename "$tmr")"
    periodic="$(grep -E '^OnUnitActiveSec=|^OnCalendar=' "$tmr" 2>/dev/null | head -1)"
    if [ -n "$periodic" ]; then
        ok "periodic: $name (${periodic%%=*})"
    else
        bad "periodic: $name" "OnUnitActiveSec and OnCalendar both absent — fires once at boot and then never again"
    fi
done

# ============================================================================
echo
echo "Restarting services have a reachable start limiter in [Unit]:"
# ============================================================================
# A Restart=always unit with RestartSec=N can never trip systemd's default 10s limiter when
# N*(burst-1) > 10, so the burst never accumulates and the unit loops instead of landing in
# failed. StartLimitIntervalSec must exceed RestartSec*(StartLimitBurst-1), and the directives
# must live in [Unit] — systemd silently ignores them under [Service].

# check_unit_limiter <file> -> 0 and a summary on stdout if the limiter is reachable; 1 and
# the reason if not; 0 with no output for a unit that does not restart.
check_unit_limiter() {
    local unit="$1" code restart rsec burst window missing sect need
    code="$(grep -vE '^[[:space:]]*#' "$unit")"
    restart="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*Restart=\([a-z-]*\).*/\1/p' | tail -1)"
    case "$restart" in always | on-failure) ;; *) return 0 ;; esac

    rsec="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*RestartSec=\([0-9][0-9]*\).*/\1/p' | tail -1)"
    burst="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*StartLimitBurst=\([0-9][0-9]*\).*/\1/p' | tail -1)"
    window="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*StartLimitIntervalSec=\([0-9][0-9]*\).*/\1/p' | tail -1)"
    missing=""
    [ -n "$rsec" ] || missing="$missing RestartSec"
    [ -n "$burst" ] || missing="$missing StartLimitBurst"
    [ -n "$window" ] || missing="$missing StartLimitIntervalSec"
    if [ -n "$missing" ]; then printf 'missing:%s\n' "$missing"; return 1; fi

    need=$((rsec * (burst - 1)))
    if [ "$window" -le "$need" ]; then
        printf '%d starts at RestartSec=%ds span %ds but StartLimitIntervalSec=%ds — limiter never fires\n' \
            "$burst" "$rsec" "$need" "$window"
        return 1
    fi

    sect="$(printf '%s\n' "$code" | awk '/^\[/{s=$0} /^[[:space:]]*StartLimit/{print s}' | sort -u)"
    if [ "$sect" != "[Unit]" ]; then
        printf 'StartLimit directives under %s, not [Unit] — systemd ignores them there\n' "${sect:-nothing}"
        return 1
    fi
    printf 'Restart=%s RestartSec=%ds burst=%d window=%ds\n' "$restart" "$rsec" "$burst" "$window"
    return 0
}

# NEGATIVE CONTROL: the checker must flag a unit whose window cannot hold burst starts
# (law-a-check-that-finds-nothing-must-first-prove-it-could-have-found-something).
_rl_tmp="$(mktemp -d)"
cat > "$_rl_tmp/unreachable-limiter.service" << 'EOF'
[Unit]
Description=Test unit with unreachable limiter
StartLimitIntervalSec=10
StartLimitBurst=5

[Service]
Type=simple
ExecStart=/bin/true
Restart=always
RestartSec=15
EOF
if check_unit_limiter "$_rl_tmp/unreachable-limiter.service" >/dev/null 2>&1; then
    bad "negative control: checker rejects unreachable limiter" "checker passed when it should fail"
else
    ok "negative control: checker rejects unreachable limiter (window=10s < 5 starts at RestartSec=15s)"
fi
rm -rf "$_rl_tmp"

_rl_found=0
for svc in "$UNIT_DIR"/*.service; do
    [ -e "$svc" ] || continue
    name="$(basename "$svc")"
    code="$(grep -vE '^[[:space:]]*#' "$svc")"
    restart="$(printf '%s\n' "$code" | sed -n 's/^[[:space:]]*Restart=\([a-z-]*\).*/\1/p' | tail -1)"
    case "$restart" in always | on-failure) ;; *) continue ;; esac
    _rl_found=$((_rl_found + 1))
    if result="$(check_unit_limiter "$svc" 2>&1)"; then
        ok "restart limiter: $name: $result"
    else
        bad "restart limiter: $name" "$result"
    fi
done
[ "$_rl_found" -gt 0 ] || bad "at least one restarting unit exists in systemd/" "none found — positive control missing"

# ============================================================================
echo
echo "WatchdogSec appears only on a non-shell ExecStart target:"
# ============================================================================
# Only the main PID's own datagram is credited by the manager; bash cannot satisfy that
# constraint (sp-3az9w). The fence checks the ExecStart TARGET only, not its arguments — a
# Rust supervisor may pass a .sh file as a child argument without that making bash the
# tracked PID.

# execstart_target <unit-body> -> the ExecStart target (first word, leading '-' stripped),
# or empty. Shared by the real loop and its controls so a control can never drift from the
# extraction it is meant to prove (the earlier version of this fence copied this case
# statement three times).
execstart_target() {
    local body="$1" line target=""
    while IFS= read -r line; do
        case "$line" in
            ExecStart=*)
                target="${line#ExecStart=}"
                target="${target#-}"
                target="${target%% *}"
                break ;;
        esac
    done <<< "$body"
    printf '%s' "$target"
}

# POSITIVE CONTROL: a synthetic bash ExecStart with WatchdogSec must be flagged.
_wd_bad="$(execstart_target '[Service]
ExecStart=/usr/bin/bash /some/script.sh
WatchdogSec=60')"
case "$_wd_bad" in
    *bash* | *.sh) ok "positive control: bash ExecStart target is extracted" ;;
    *) bad "positive control: bash ExecStart target is extracted" "got [$_wd_bad]" ;;
esac

# NEGATIVE CONTROL: a Rust binary passing a .sh argument must not be flagged as its target.
_wd_ok="$(execstart_target '[Service]
ExecStart=/usr/local/bin/spira-supervise /path/collect.sh loop
WatchdogSec=60')"
case "$_wd_ok" in
    *bash* | *.sh) bad "negative control: Rust supervisor with .sh argument is not flagged" "got [$_wd_ok]" ;;
    *) ok "negative control: Rust supervisor with .sh argument is not flagged" ;;
esac

_wd_checked=0
for svc in "$UNIT_DIR"/*.service; do
    [ -e "$svc" ] || continue
    name="$(basename "$svc")"
    body="$(grep -v '^[[:space:]]*#' "$svc")"
    target="$(execstart_target "$body")"
    case "$target" in *bash* | *.sh) ;; *) continue ;; esac
    _wd_checked=$((_wd_checked + 1))
    nowant "watchdog: $name bash ExecStart target has no WatchdogSec" "WatchdogSec=" "$body"
done
[ "$_wd_checked" -gt 0 ] || bad "at least one bash/shell ExecStart unit was examined" "none found — are ExecStart patterns still .sh or bash?"

# spira-cockpit.service is the one unit that MUST carry WatchdogSec (sp-mplcb wires
# spira-supervise, a Rust binary, as its ExecStart, so the fence above does not apply to it).
_wd_cockpit="$UNIT_DIR/spira-cockpit.service"
if [ -r "$_wd_cockpit" ]; then
    want "spira-cockpit.service: WatchdogSec present (Rust supervisor is main PID)" \
        "WatchdogSec=" "$(grep -v '^[[:space:]]*#' "$_wd_cockpit")"
else
    bad "spira-cockpit.service is readable" "not found at $_wd_cockpit"
fi

# ============================================================================
echo
echo "The verdict service: driver wiring, TimeoutStartSec>=3600, no CPUQuota:"
# ============================================================================
# defect sp-tv7ue: nothing proved the service called queue.sh step (which settles the open
# batch then opens the next) rather than verdict.sh alone (which would skip batch.sh and
# leave no new batch after a landing). defect sp-vhvyi: TimeoutStartSec was too short for a
# full red-batch replay, and CPUQuota would throttle one if it were ever added.
_vd_svc="$UNIT_DIR/spira-verdict.service"
_vd_driver="$HERE/spira-verdict.sh"

if [ -r "$_vd_svc" ]; then
    _vd_execstart="$(grep '^ExecStart=' "$_vd_svc" 2>/dev/null | head -1)"
    if [ -n "$_vd_execstart" ]; then
        want "spira-verdict.service ExecStart invokes spira-verdict.sh" "spira-verdict.sh" "$_vd_execstart"
    else
        bad "spira-verdict.service has an ExecStart line" "none found"
    fi

    # POSITIVE CONTROL for the Timeout/CPUQuota checks below: a readable Type= line proves
    # the file is non-empty before any absence verdict about CPUQuota is trusted.
    if grep -q '^Type=' "$_vd_svc" 2>/dev/null; then
        ok "positive control: spira-verdict.service Type= is present (file is readable)"
    else
        bad "positive control: spira-verdict.service Type= is present" "not found — file may be empty or unparseable"
    fi

    _vd_timeout="$(grep '^TimeoutStartSec=' "$_vd_svc" 2>/dev/null | head -1)"
    if [ -n "$_vd_timeout" ]; then
        _vd_timeout_val="${_vd_timeout#TimeoutStartSec=}"
        if [ "${_vd_timeout_val}" -ge 3600 ] 2>/dev/null; then
            ok "spira-verdict.service TimeoutStartSec >= 3600s (covers red-batch replay per member)"
        else
            bad "spira-verdict.service TimeoutStartSec >= 3600s" "${_vd_timeout_val}s < 3600s — systemd kills every replay at two minutes (sp-vhvyi)"
        fi
    else
        bad "spira-verdict.service has TimeoutStartSec" "directive absent"
    fi

    if grep -q '^CPUQuota=' "$_vd_svc" 2>/dev/null; then
        bad "spira-verdict.service has no CPUQuota (replay runs at full CPU)" \
            "found CPUQuota — throttles containers during red-batch replay (sp-vhvyi)"
    else
        ok "spira-verdict.service has no CPUQuota (replay runs at full CPU)"
    fi
else
    bad "spira-verdict.service is readable" "not found at $_vd_svc"
fi

if [ -r "$_vd_driver" ]; then
    _vd_src="$(cat "$_vd_driver")"
    if [ -n "$_vd_src" ]; then
        # POSITIVE CONTROL: a script sourcing nothing cannot call spira_repos or repo_land.
        want "positive control: spira-verdict.sh sources lib.sh" "lib.sh" "$_vd_src"
        want "spira-verdict.sh calls queue.sh step" 'queue.sh" step' "$_vd_src"
        want "spira-verdict.sh iterates repos" "spira_repos" "$_vd_src"
    else
        bad "spira-verdict.sh is non-empty" "empty or unreadable"
    fi
else
    bad "spira-verdict.sh is readable" "not found at $_vd_driver"
fi

# ============================================================================
echo
echo "render.py renders shared units against the release root, not the git checkout:"
# ============================================================================
# In a split-checkout deployment SPIRA_REPO is the git checkout and SPIRA_PROD is the
# release tree; an ExecStart using @SPIRA_REPO@ points into the checkout, which a release
# activation does not update. @SPIRA_PROD_ROOT@ (dirname of SPIRA_PROD) is the placeholder
# that resolves into the release tree instead.

_rd_tmp="$(mktemp -d)"
_rd_repo="$_rd_tmp/repo"
_rd_prod="$_rd_tmp/releases/current/spira"
_rd_prod_root="$_rd_tmp/releases/current"
mkdir -p "$_rd_repo" "$_rd_prod" "$_rd_prod_root/cockpit"

# Render using render.py directly — the actual module install.sh and unit-ensure.sh both
# call, not a copy of its substitution logic.
render_unit() {
    python3 "$UNIT_DIR/render.py" "$1" \
        --home "$_rd_repo" --repo "$_rd_repo" --run "$_rd_tmp/run" --db "$_rd_tmp/db" \
        --cockpit "$_rd_prod_root/cockpit" --prod "$_rd_prod" --instance test
}

# POSITIVE CONTROL: a synthetic @SPIRA_REPO@ template must render to the checkout path, so
# the detector is proven to fire before its silence on the real templates is trusted.
_rd_bad="$_rd_tmp/bad.service"
printf '[Service]\nExecStart=@SPIRA_REPO@/something.sh\n' > "$_rd_bad"
want "positive control: @SPIRA_REPO@ renders to checkout path — detector fires" \
    "ExecStart=$_rd_repo/" "$(render_unit "$_rd_bad")"

for svc in concierge.service beads-push.service; do
    _rd_execline="$(grep -E '^ExecStart=' "$UNIT_DIR/$svc")"
    nowant "$svc: ExecStart does not use @SPIRA_REPO@" "@SPIRA_REPO@" "$_rd_execline"
    want "$svc: ExecStart uses @SPIRA_PROD_ROOT@" "@SPIRA_PROD_ROOT@" "$_rd_execline"

    _rd_rendered="$(render_unit "$UNIT_DIR/$svc" | grep -E '^ExecStart=')"
    nowant "$svc: rendered ExecStart does not point into git checkout" "$_rd_repo" "$_rd_rendered"
    want "$svc: rendered ExecStart points into release root" "$_rd_prod_root" "$_rd_rendered"
done
rm -rf "$_rd_tmp"

# ============================================================================
echo
echo "ExecStart @*_BIN@ tokens (OPTIONAL derived from units.sh, not a hand-copied list):"
# ============================================================================
# The old test-tarball-bins.sh hand-copied its list of OPTIONAL unit names; it silently fell
# out of sync with units.sh (missing spira-landing-pass.service/.timer). Reusing is_optional
# from the units.sh parse above means this scan and the UNITS/_ENABLE_TMPL checks above can
# never disagree about which units are conditional.

_tb_found=0
for svc in "$UNIT_DIR"/*.service; do
    [ -e "$svc" ] || continue
    name="$(basename "$svc")"
    is_optional "$name" && continue
    for tok in $(grep '^ExecStart=' "$svc" 2>/dev/null | grep -oE '@[A-Z_]+_BIN@' | tr -d '@'); do
        _tb_found=$((_tb_found + 1))
        ok "non-optional $name references @${tok}@ in ExecStart"
    done
done
[ "$_tb_found" -gt 0 ] \
    && ok "positive control: at least one @*_BIN@ token found in a non-optional unit's ExecStart" \
    || bad "at least one @*_BIN@ token in non-optional ExecStart" \
        "none found — either all units are optional or ExecStart references were removed; positive control failed"

tl_summary
